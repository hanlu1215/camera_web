use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use chrono::Local;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::state::{AppState, ConvertJobStatus};

const KEEP_FINISHED_JOBS: usize = 60;

#[derive(Debug, Clone)]
pub struct ConvertRequest {
    pub dir: PathBuf,
    pub fps: f64,
    pub key: String,
}

struct ImageEntry {
    path: PathBuf,
    modified: SystemTime,
}

struct JobOutcome {
    output: PathBuf,
    images: usize,
    message: String,
}

pub async fn convert_worker(state: Arc<AppState>, mut rx: mpsc::UnboundedReceiver<ConvertRequest>) {
    while let Some(request) = rx.recv().await {
        let label = request.dir.display().to_string();
        set_status(&state, &request.key, |status| {
            status.state = "running".to_string();
            status.message = "正在转换…".to_string();
            status.started_at = Some(now_string());
            status.finished_at = None;
        });
        info!("conversion job started: {label}");

        let result = convert_folder(&state, &request.dir, request.fps).await;

        match result {
            Ok(outcome) => {
                set_status(&state, &request.key, |status| {
                    status.state = "done".to_string();
                    status.output = relative_key(&state.config.recordings_dir, &outcome.output);
                    status.images = outcome.images;
                    status.message = outcome.message;
                    status.finished_at = Some(now_string());
                });
                info!("conversion done: {label} -> {}", outcome.output.display());
            }
            Err(err) => {
                warn!("conversion failed for {label}: {err:#}");
                set_status(&state, &request.key, |status| {
                    status.state = "error".to_string();
                    status.message = format!("{err:#}");
                    status.finished_at = Some(now_string());
                });
            }
        }
        state.bump_history();
    }
}

async fn convert_folder(
    state: &Arc<AppState>,
    dir: &Path,
    fps: f64,
) -> anyhow::Result<JobOutcome> {
    let mut images = collect_images(dir).context("无法读取图片列表")?;
    if images.is_empty() {
        bail!("文件夹中没有 JPG 图片");
    }
    // Sort by file time: the 6-digit counter wraps, so names are not always
    // chronological. Name is only a tie-breaker.
    images.sort_by(|a, b| {
        a.modified
            .cmp(&b.modified)
            .then_with(|| a.path.cmp(&b.path))
    });

    let folder_name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "recording".to_string());
    // The finished video lives in recordings/Video/. The source folder is
    // removed only after the conversion has been verified.
    let video_dir = state.config.video_dir.clone();
    tokio::fs::create_dir_all(&video_dir)
        .await
        .context("无法创建导出视频目录")?;
    let output = video_dir.join(format!("{folder_name}.mp4"));
    let temp = video_dir.join(format!(".{folder_name}.part"));
    let list_path = dir.join(".ffmpeg-concat.txt");

    let fps = fps.clamp(0.05, 120.0);
    let frame_duration = 1.0 / fps;
    write_concat_list(&list_path, &images, frame_duration).context("无法写入转换清单")?;

    let ffmpeg = state.config.ffmpeg.clone();
    let result = run_ffmpeg(state, &ffmpeg, &list_path, &temp).await;

    if let Err(err) = result {
        let _ = tokio::fs::remove_file(&temp).await;
        let _ = tokio::fs::remove_file(&list_path).await;
        return Err(err);
    }

    if let Err(err) = verify_video(&ffmpeg, &temp).await {
        let _ = tokio::fs::remove_file(&temp).await;
        let _ = tokio::fs::remove_file(&list_path).await;
        return Err(err);
    }

    if tokio::fs::metadata(&output).await.is_ok() {
        let _ = tokio::fs::remove_file(&output).await;
    }
    tokio::fs::rename(&temp, &output)
        .await
        .context("无法把临时视频重命名为最终文件")?;

    // Remove the source folder (including the concat list and any leftover
    // files) only after the MP4 is in place and verified.
    let total = images.len();
    let output_name = output
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let message = match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => format!(
            "已转换为 Video/{output_name}，删除原录制文件夹（{total} 张原图）"
        ),
        Err(err) => {
            warn!("cannot remove converted folder {}: {err}", dir.display());
            format!("已转换为 Video/{output_name}，但删除原录制文件夹失败: {err}")
        }
    };

    Ok(JobOutcome {
        output,
        images: total,
        message,
    })
}

fn collect_images(dir: &Path) -> io::Result<Vec<ImageEntry>> {
    let mut images = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if !is_jpeg(&path) {
            continue;
        }
        let modified = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .unwrap_or(UNIX_EPOCH);
        images.push(ImageEntry { path, modified });
    }
    Ok(images)
}

fn is_jpeg(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("jpg") | Some("jpeg")
    )
}

fn write_concat_list(path: &Path, images: &[ImageEntry], duration: f64) -> io::Result<()> {
    let mut text = String::with_capacity(images.len() * 80);
    text.push_str("ffconcat version 1.0\n");
    for image in images {
        let absolute = std::fs::canonicalize(&image.path).unwrap_or_else(|_| image.path.clone());
        text.push_str(&format!("file '{}'\n", escape_concat_path(&absolute)));
        text.push_str(&format!("duration {duration:.6}\n"));
    }
    // The concat demuxer wants the final frame repeated so its duration counts.
    if let Some(last) = images.last() {
        let absolute = std::fs::canonicalize(&last.path).unwrap_or_else(|_| last.path.clone());
        text.push_str(&format!("file '{}'\n", escape_concat_path(&absolute)));
    }
    std::fs::write(path, text)
}

fn escape_concat_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}

async fn run_ffmpeg(
    state: &AppState,
    ffmpeg: &str,
    list_path: &Path,
    temp: &Path,
) -> anyhow::Result<()> {
    let output = Command::new(ffmpeg)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-nostdin")
        .arg("-y")
        .arg("-f")
        .arg("concat")
        .arg("-safe")
        .arg("0")
        .arg("-i")
        .arg(list_path)
        .arg("-an")
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("veryfast")
        .arg("-crf")
        .arg("23")
        .arg("-vf")
        .arg("scale=out_range=tv,format=yuv420p")
        .arg("-threads")
        .arg(state.config.ffmpeg_threads.max(1).to_string())
        .arg("-movflags")
        .arg("+faststart")
        .arg("-f")
        .arg("mp4")
        .arg(temp)
        .output()
        .await
        .with_context(|| format!("无法启动 ffmpeg（{ffmpeg}），请先安装 ffmpeg"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail: String = stderr.trim().chars().take(800).collect();
        bail!("ffmpeg 转换失败: {detail}");
    }
    Ok(())
}

async fn verify_video(ffmpeg: &str, path: &Path) -> anyhow::Result<()> {
    let metadata = tokio::fs::metadata(path)
        .await
        .context("转换后找不到视频文件")?;
    if metadata.len() < 1024 {
        bail!("转换生成的视频文件异常（{} 字节）", metadata.len());
    }

    let ffprobe = ffprobe_path(ffmpeg);
    let probe = Command::new(&ffprobe)
        .arg("-v")
        .arg("error")
        .arg("-select_streams")
        .arg("v:0")
        .arg("-show_entries")
        .arg("stream=codec_name")
        .arg("-of")
        .arg("default=nw=1:nk=1")
        .arg(path)
        .output()
        .await;

    match probe {
        Ok(output) => {
            if !output.status.success() {
                bail!(
                    "ffprobe 校验失败: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            let codec = String::from_utf8_lossy(&output.stdout);
            if codec.trim().is_empty() {
                bail!("转换结果中没有视频流，已保留原图");
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // ffprobe is optional; the size check above still applies.
            warn!("ffprobe not found, skipping codec verification");
        }
        Err(err) => warn!("ffprobe failed, skipping codec verification: {err}"),
    }
    Ok(())
}

fn ffprobe_path(ffmpeg: &str) -> String {
    let path = Path::new(ffmpeg);
    if path.components().count() > 1 {
        if let Some(parent) = path.parent() {
            return parent.join("ffprobe").to_string_lossy().into_owned();
        }
    }
    "ffprobe".to_string()
}

pub fn enqueue(state: &Arc<AppState>, dir: PathBuf, fps: f64) -> Result<(), String> {
    let key = relative_key(&state.config.recordings_dir, &dir);
    reserve_job(state, &key, "该文件夹已经在转换队列中")?;
    state
        .convert_tx
        .send(ConvertRequest {
            dir,
            fps: fps.clamp(0.05, 120.0),
            key,
        })
        .map_err(|_| "转换队列已关闭".to_string())
}

fn reserve_job(state: &AppState, key: &str, busy_message: &str) -> Result<(), String> {
    let mut jobs = state.conversions.lock().unwrap();
    if let Some(job) = jobs.get(key) {
        if job.state == "queued" || job.state == "running" {
            return Err(busy_message.to_string());
        }
    }
    jobs.insert(key.to_string(), ConvertJobStatus::new(key.to_string()));
    trim_jobs(&mut jobs);
    Ok(())
}

fn trim_jobs(jobs: &mut std::collections::HashMap<String, ConvertJobStatus>) {
    if jobs.len() <= KEEP_FINISHED_JOBS + 20 {
        return;
    }
    let stale: Vec<String> = jobs
        .iter()
        .filter(|(_, job)| job.state == "done" || job.state == "error")
        .map(|(key, _)| key.clone())
        .take(jobs.len().saturating_sub(KEEP_FINISHED_JOBS))
        .collect();
    for key in stale {
        jobs.remove(&key);
    }
}

fn set_status(state: &AppState, key: &str, update: impl FnOnce(&mut ConvertJobStatus)) {
    let mut jobs = state.conversions.lock().unwrap();
    if let Some(job) = jobs.get_mut(key) {
        update(job);
    }
}

/// All closed folders that still contain JPG images, sorted by name.
pub fn pending_folders(state: &AppState) -> Vec<PathBuf> {
    let active: Vec<PathBuf> = state
        .active_dirs()
        .into_iter()
        .map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir))
        .collect();

    let mut folders = Vec::new();
    let entries = match std::fs::read_dir(&state.config.recordings_dir) {
        Ok(entries) => entries,
        Err(_) => return folders,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if active.contains(&canonical) {
            continue;
        }
        let has_images = std::fs::read_dir(&path)
            .map(|entries| entries.flatten().any(|entry| is_jpeg(&entry.path())))
            .unwrap_or(false);
        if has_images {
            folders.push(path);
        }
    }
    folders.sort();
    folders
}

pub fn relative_key(root: &Path, path: &Path) -> String {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    path.strip_prefix(&root)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn now_string() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Canonicalize a user-supplied folder under the recordings root and reject
/// anything that escapes it.
pub fn resolve_recording_dir(root: &Path, requested: &str) -> anyhow::Result<PathBuf> {
    let requested = requested.trim().trim_start_matches('/');
    if requested.is_empty() {
        bail!("缺少文件夹路径");
    }
    let root = std::fs::canonicalize(root).context("录像根目录不存在")?;
    let candidate = root.join(requested);
    let canonical = std::fs::canonicalize(&candidate).context("文件夹不存在")?;
    if !canonical.starts_with(&root) {
        bail!("非法路径");
    }
    if !canonical.is_dir() {
        bail!("路径不是文件夹");
    }
    Ok(canonical)
}

/// Resolve a video reference to a real path and its job/status key.
///
/// `@video/<relative>` refers to the configurable export directory (which may
/// live outside recordings); anything else is relative to the recordings root.
pub fn resolve_video_ref(
    recordings_root: &Path,
    video_dir: &Path,
    requested: &str,
) -> anyhow::Result<(PathBuf, String)> {
    let requested = requested.trim();
    if requested.is_empty() {
        bail!("缺少视频路径");
    }

    let (base, relative, fixed_key) = if let Some(rest) = requested.strip_prefix("@video/") {
        (video_dir.to_path_buf(), rest.to_string(), requested.to_string())
    } else {
        (
            recordings_root.to_path_buf(),
            requested
                .trim_start_matches('/')
                .trim_start_matches("@rec/")
                .to_string(),
            String::new(),
        )
    };

    if relative.is_empty() {
        bail!("缺少视频路径");
    }
    let base = std::fs::canonicalize(&base).context("视频目录不存在")?;
    let canonical = std::fs::canonicalize(base.join(&relative)).context("视频不存在")?;
    if !canonical.starts_with(&base) {
        bail!("非法路径");
    }
    if !canonical.is_file() {
        bail!("路径不是文件");
    }
    let name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !name.ends_with(".mp4") {
        bail!("只支持 MP4 视频");
    }
    if name.ends_with(".orig.mp4") {
        bail!("不能转码备份文件");
    }

    let key = if fixed_key.is_empty() {
        relative_key(recordings_root, &canonical)
    } else {
        fixed_key
    };
    Ok((canonical, key))
}

pub fn ffmpeg_available(ffmpeg: &str) -> bool {
    std::process::Command::new(ffmpeg)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
