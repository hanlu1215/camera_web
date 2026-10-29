use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use serde::Serialize;

use crate::state::AppState;

const CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq)]
enum VideoScope {
    Recording,
    VideoDir,
}

#[derive(Clone, Debug, Serialize)]
pub struct VideoInfo {
    pub name: String,
    /// API path: `@video/<rel>` for the export directory, otherwise relative
    /// to the recordings root.
    pub path: String,
    /// Absolute URL for `<video src>` (`/media/...` or `/media-video/...`).
    pub url: String,
    pub size: u64,
    pub modified: String,
    pub modified_ts: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct FolderInfo {
    pub name: String,
    pub path: String,
    /// motion | snapshot | video | folder
    pub kind: String,
    pub image_count: usize,
    pub videos: Vec<VideoInfo>,
    /// First image (relative path) for a list thumbnail.
    pub thumb: Option<String>,
    pub modified: String,
    pub modified_ts: u64,
    pub active: bool,
    pub converting: bool,
    pub conversion_state: String,
    pub conversion_message: String,
}

pub fn scan(state: &AppState) -> Vec<FolderInfo> {
    let cached = {
        let cache = state.history_cache.lock().unwrap();
        match cache.as_ref() {
            Some((time, folders)) if time.elapsed() < CACHE_TTL => Some(folders.clone()),
            _ => None,
        }
    };

    let folders = match cached {
        Some(folders) => folders,
        None => {
            let folders = scan_folders(state, &state.config.recordings_dir);
            *state.history_cache.lock().unwrap() = Some((Instant::now(), folders.clone()));
            folders
        }
    };

    decorate(state, folders)
}

fn decorate(state: &AppState, mut folders: Vec<FolderInfo>) -> Vec<FolderInfo> {
    let active: Vec<PathBuf> = state
        .active_dirs()
        .into_iter()
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
        .collect();

    {
        let jobs = state.conversions.lock().unwrap();
        for folder in &mut folders {
            let full = state.config.recordings_dir.join(&folder.path);
            let canonical = std::fs::canonicalize(&full).unwrap_or(full);
            folder.active = active.contains(&canonical);

            if let Some(job) = jobs.get(&folder.path) {
                folder.converting = job.state == "queued" || job.state == "running";
                folder.conversion_state = job.state.clone();
                folder.conversion_message = job.message.clone();
            }
        }
    }

    folders.sort_by_key(|folder| std::cmp::Reverse(folder.modified_ts));
    folders
}

fn scan_folders(state: &AppState, root: &Path) -> Vec<FolderInfo> {
    let mut folders = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return folders,
    };
    let video_dir = state.config.video_dir.clone();
    let video_canonical = std::fs::canonicalize(&video_dir).ok();
    let root_canonical = std::fs::canonicalize(root).ok();
    let video_is_root = matches!(
        (&video_canonical, &root_canonical),
        (Some(video), Some(root)) if video == root
    );

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            // Root-level MP4s are listed here unless video_dir itself is the
            // recordings root, in which case the synthetic group covers them.
            if !video_is_root && is_listable_video(&path) {
                if let Some(video) = video_info(root, &path, VideoScope::Recording, &video_dir) {
                    let (modified, modified_ts) = format_time(video_modified(&path));
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let stem = path
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_else(|| name.clone());
                    let kind = if stem.starts_with("motion_") {
                        "motion"
                    } else if looks_like_session(&stem) {
                        "snapshot"
                    } else {
                        "video"
                    };
                    folders.push(FolderInfo {
                        name: name.clone(),
                        path: name,
                        kind: kind.to_string(),
                        image_count: 0,
                        videos: vec![video],
                        thumb: None,
                        modified,
                        modified_ts,
                        active: false,
                        converting: false,
                        conversion_state: String::new(),
                        conversion_message: String::new(),
                    });
                }
            }
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        // The export directory is presented as a separate synthetic group so
        // it works whether it is inside recordings/ or on another disk.
        if video_canonical
            .as_ref()
            .map(|video| {
                std::fs::canonicalize(&path)
                    .map(|path| path == *video)
                    .unwrap_or(false)
            })
            .unwrap_or(false)
        {
            continue;
        }

        let mut image_count = 0usize;
        let mut videos = Vec::new();
        let mut newest = UNIX_EPOCH;
        let mut thumb = None;

        if let Ok(children) = std::fs::read_dir(&path) {
            for child in children.flatten() {
                // Count by extension only: calling is_dir()/is_file() here
                // would issue one stat per entry (~280k on a long-running
                // folder) and make the history request take minutes.
                let child_path = child.path();
                if is_jpeg(&child_path) {
                    image_count += 1;
                    if thumb.is_none() {
                        thumb = Some(relative(root, &child_path));
                    }
                } else if is_listable_video(&child_path) {
                    if let Some(video) =
                        video_info(root, &child_path, VideoScope::Recording, &video_dir)
                    {
                        newest = newest.max(video_modified(&child_path));
                        videos.push(video);
                    }
                }
            }
        }

        // A container such as `Video/` may hold sub-folders of videos.
        if image_count == 0 && videos.is_empty() {
            if let Ok(children) = std::fs::read_dir(&path) {
                for child in children.flatten() {
                    let child_path = child.path();
                    if !child_path.is_dir() {
                        continue;
                    }
                    if let Ok(grandchildren) = std::fs::read_dir(&child_path) {
                        for grandchild in grandchildren.flatten() {
                            let file = grandchild.path();
                            if is_listable_video(&file) {
                                if let Some(video) = video_info(
                                    root,
                                    &file,
                                    VideoScope::Recording,
                                    &video_dir,
                                ) {
                                    newest = newest.max(video_modified(&file));
                                    videos.push(video);
                                }
                            }
                        }
                    }
                }
            }
        }

        if image_count == 0 && videos.is_empty() {
            continue;
        }

        let dir_modified = video_modified(&path);
        if dir_modified > newest {
            newest = dir_modified;
        }
        let (modified, modified_ts) = format_time(newest);
        let kind = if name.starts_with("motion_") {
            "motion"
        } else if image_count > 0 || looks_like_session(&name) {
            "snapshot"
        } else {
            "video"
        };

        folders.push(FolderInfo {
            name,
            path: relative(root, &path),
            kind: kind.to_string(),
            image_count,
            videos,
            thumb,
            modified,
            modified_ts,
            active: false,
            converting: false,
            conversion_state: String::new(),
            conversion_message: String::new(),
        });
    }

    if let Some(group) = scan_video_group(&video_dir) {
        folders.push(group);
    }

    folders
}

/// The configurable export directory, presented as a single "videos" group.
fn scan_video_group(video_dir: &Path) -> Option<FolderInfo> {
    let entries = std::fs::read_dir(video_dir).ok()?;
    let mut videos = Vec::new();
    let mut newest = UNIX_EPOCH;
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_listable_video(&path) {
            continue;
        }
        if let Some(video) = video_info(video_dir, &path, VideoScope::VideoDir, video_dir) {
            newest = newest.max(video_modified(&path));
            videos.push(video);
        }
    }
    if videos.is_empty() {
        return None;
    }

    let (modified, modified_ts) = format_time(newest);
    let name = video_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Video".to_string());
    Some(FolderInfo {
        name,
        path: "@video".to_string(),
        kind: "video".to_string(),
        image_count: 0,
        videos,
        thumb: None,
        modified,
        modified_ts,
        active: false,
        converting: false,
        conversion_state: String::new(),
        conversion_message: String::new(),
    })
}

fn video_info(
    root: &Path,
    path: &Path,
    scope: VideoScope,
    video_dir: &Path,
) -> Option<VideoInfo> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    let (modified_text, modified_ts) = format_time(modified);
    let relative_path = match scope {
        VideoScope::Recording => relative(root, path),
        VideoScope::VideoDir => relative(video_dir, path),
    };
    let (api_path, url) = match scope {
        VideoScope::Recording => (
            relative_path.clone(),
            format!("/media/{}", encode_path(&relative_path)),
        ),
        VideoScope::VideoDir => (
            format!("@video/{relative_path}"),
            format!("/media-video/{}", encode_path(&relative_path)),
        ),
    };
    Some(VideoInfo {
        name: path.file_name()?.to_string_lossy().into_owned(),
        path: api_path,
        url,
        size: metadata.len(),
        modified: modified_text,
        modified_ts,
    })
}

fn video_modified(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .unwrap_or(UNIX_EPOCH)
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn encode_path(relative: &str) -> String {
    relative
        .split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn encode_segment(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn format_time(time: SystemTime) -> (String, u64) {
    let datetime: DateTime<Local> = time.into();
    (
        datetime.format("%Y-%m-%d %H:%M:%S").to_string(),
        datetime.timestamp().max(0) as u64,
    )
}

/// Session folders are named `<YYYYmmdd>_<HHMMSS>[_N]`.
fn looks_like_session(name: &str) -> bool {
    let mut parts = name.split('_');
    let date = parts.next().unwrap_or("");
    let time = parts.next().unwrap_or("");
    date.len() == 8
        && time.len() == 6
        && date.bytes().all(|byte| byte.is_ascii_digit())
        && time.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_jpeg(path: &Path) -> bool {
    // Extension-only (no filesystem call); directories are not expected to
    // have image/video extensions inside our own recording folders.
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("jpg") | Some("jpeg")
    )
}

fn is_mp4(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("mp4")
    )
}

/// MP4s that should be shown: skip hidden temp files and legacy
/// `<stem>.orig.mp4` backups.
fn is_listable_video(path: &Path) -> bool {
    if !is_mp4(path) {
        return false;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    !name.starts_with('.') && !name.ends_with(".orig.mp4")
}
