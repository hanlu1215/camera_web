use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Local;
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::ServeDir;
use tracing::warn;

use crate::convert;
use crate::recording;
use crate::scheduler;
use crate::state::{parse_hhmm, AppState};

pub fn router(state: Arc<AppState>) -> Router {
    let media = ServeDir::new(state.config.recordings_dir.clone());
    let videos = ServeDir::new(state.config.video_dir.clone());
    Router::new()
        .route("/", get(index))
        .route("/video_feed", get(video_feed))
        // ── legacy endpoints kept for existing scripts / bookmarks ──────────
        .route("/status", get(status))
        .route("/recording_status", get(recording_status))
        .route("/start_recording", get(start_recording))
        .route("/stop_recording", get(stop_recording))
        .route("/start_motion_recording", get(start_motion_recording))
        .route("/stop_motion_recording", get(stop_motion_recording))
        .route("/motion_recording_status", get(motion_recording_status))
        .route("/set_recording_interval", get(set_interval_query))
        // ── JSON API ────────────────────────────────────────────────────────
        .route("/api/status", get(status))
        .route("/api/recording/status", get(recording_status))
        .route("/api/recording/start", post(start_recording))
        .route("/api/recording/stop", post(stop_recording))
        .route("/api/recording/interval", post(set_interval_json))
        .route("/api/motion_recording/start", post(start_motion_recording))
        .route("/api/motion_recording/stop", post(stop_motion_recording))
        .route("/api/motion_recording_status", get(motion_recording_status))
        .route("/api/history", get(history))
        .route("/api/convert", post(convert_one))
        .route("/api/convert/all", post(convert_all))
        .route("/api/delete", post(delete_entry))
        .route("/api/conversions", get(conversions))
        .route("/api/settings", get(get_settings).post(set_settings))
        .route("/api/health", get(health))
        .nest_service("/media", media)
        .nest_service("/media-video", videos)
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("ui.html"))
}

struct ClientGuard {
    state: Arc<AppState>,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        let _serial = self.state.client_lock.lock().unwrap();
        let _ = self.state.active_clients.try_update(
            Ordering::SeqCst,
            Ordering::SeqCst,
            |count| count.checked_sub(1),
        );
        self.state.maybe_release_camera();
    }
}

async fn video_feed(State(state): State<Arc<AppState>>) -> Response {
    {
        let _serial = state.client_lock.lock().unwrap();
        state.active_clients.fetch_add(1, Ordering::SeqCst);
        state.ensure_camera_started();
    }
    let guard = ClientGuard {
        state: Arc::clone(&state),
    };

    let stream = async_stream::stream! {
        let _guard = guard;
        let mut last_seq = u64::MAX;
        loop {
            match state.latest_frame() {
                Some(frame) => {
                    if frame.seq != last_seq {
                        last_seq = frame.seq;
                        let header = format!(
                            "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                            frame.jpeg.len()
                        );
                        yield Ok::<Bytes, Infallible>(Bytes::from(header));
                        yield Ok(frame.jpeg.clone());
                        yield Ok(Bytes::from_static(b"\r\n"));
                    }
                }
                None => state.ensure_camera_started(),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            "multipart/x-mixed-replace; boundary=frame",
        )
        .header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate")
        .body(Body::from_stream(stream))
        .expect("valid MJPEG response")
}

async fn status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let frame_age = state
        .latest_frame()
        .and_then(|frame| frame.captured_at.elapsed().ok())
        .map(|age| age.as_secs_f64());
    Json(json!({
        "motion": state.motion.load(Ordering::SeqCst),
        "camera_active": state.camera_keep_alive.load(Ordering::SeqCst),
        "clients": state.active_clients.load(Ordering::SeqCst),
        "frame_age_secs": frame_age,
    }))
}

async fn recording_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let settings = state.settings_snapshot();
    let (recording, dir) = {
        let rec = state.rec.lock().unwrap();
        (rec.active, rec.dir.clone())
    };
    let (motion_recording, motion_dir) = {
        let rec = state.motion_rec.lock().unwrap();
        (rec.active, rec.dir.clone())
    };

    Json(json!({
        "recording": recording,
        "dir": dir.map(|p| p.display().to_string()).unwrap_or_default(),
        "interval": settings.recording_interval,
        "motion_recording": motion_recording,
        "motion_dir": motion_dir.map(|p| p.display().to_string()).unwrap_or_default(),
        "auto_convert": {
            "enabled": settings.auto_convert_enabled,
            "time": settings.auto_convert_time,
            "fps": settings.convert_fps,
        },
        "recordings_dir": state.config.recordings_dir.display().to_string(),
        "video_dir": state.config.video_dir.display().to_string(),
        "ffmpeg_available": state.ffmpeg_available,
    }))
}

async fn start_recording(State(state): State<Arc<AppState>>) -> Json<Value> {
    state.ensure_camera_started();
    match recording::start_interval(&state) {
        Ok(dir) => Json(json!({"started": true, "dir": dir.display().to_string()})),
        Err(err) => Json(json!({"started": false, "error": err.to_string()})),
    }
}

async fn stop_recording(State(state): State<Arc<AppState>>) -> Json<Value> {
    recording::stop_interval(&state);
    state.maybe_release_camera();
    Json(json!({"stopped": true}))
}

async fn start_motion_recording(State(state): State<Arc<AppState>>) -> Json<Value> {
    state.ensure_camera_started();
    match recording::start_motion(&state) {
        Ok(dir) => Json(json!({"started": true, "dir": dir.display().to_string()})),
        Err(err) => Json(json!({"started": false, "error": err.to_string()})),
    }
}

async fn stop_motion_recording(State(state): State<Arc<AppState>>) -> Json<Value> {
    recording::stop_motion(&state);
    state.maybe_release_camera();
    Json(json!({"stopped": true}))
}

async fn motion_recording_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let (active, dir) = {
        let rec = state.motion_rec.lock().unwrap();
        (rec.active, rec.dir.clone())
    };
    Json(json!({
        "motion_recording": active,
        "dir": dir.map(|p| p.display().to_string()).unwrap_or_default(),
        "motion_detected": state.motion.load(Ordering::SeqCst),
    }))
}

#[derive(Deserialize)]
struct IntervalQuery {
    interval: Option<f64>,
}

async fn set_interval_query(
    State(state): State<Arc<AppState>>,
    Query(query): Query<IntervalQuery>,
) -> Response {
    match query.interval {
        Some(value) => match apply_interval(&state, value) {
            Ok(interval) => Json(json!({"success": true, "interval": interval})).into_response(),
            Err(message) => api_error(StatusCode::BAD_REQUEST, message),
        },
        None => api_error(
            StatusCode::BAD_REQUEST,
            "missing interval parameter".to_string(),
        ),
    }
}

#[derive(Deserialize)]
struct IntervalBody {
    interval: f64,
}

async fn set_interval_json(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IntervalBody>,
) -> Response {
    match apply_interval(&state, body.interval) {
        Ok(interval) => Json(json!({"success": true, "interval": interval})).into_response(),
        Err(message) => api_error(StatusCode::BAD_REQUEST, message),
    }
}

fn apply_interval(state: &Arc<AppState>, value: f64) -> Result<f64, String> {
    if !value.is_finite() || value < 0.01 {
        return Err("interval must be >= 0.01".to_string());
    }
    {
        let mut settings = state.settings.lock().unwrap();
        settings.recording_interval = value;
        settings.normalize();
        settings
            .save(&state.settings_path)
            .map_err(|err| format!("cannot save settings: {err}"))?;
        state
            .interval_ms
            .store(settings.interval_ms(), Ordering::Relaxed);
    }
    Ok(value)
}

async fn history(State(state): State<Arc<AppState>>) -> Response {
    let scan_state = Arc::clone(&state);
    let folders = match tokio::task::spawn_blocking(move || crate::history::scan(&scan_state)).await {
        Ok(folders) => folders,
        Err(err) => {
            warn!("history scan failed: {err}");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot scan history");
        }
    };
    Json(json!({
        "folders": folders,
        "server_time": Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct ConvertBody {
    path: String,
    #[serde(default)]
    fps: Option<f64>,
}

async fn convert_one(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ConvertBody>,
) -> Response {
    let dir = match convert::resolve_recording_dir(&state.config.recordings_dir, &body.path) {
        Ok(dir) => dir,
        Err(err) => return api_error(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };

    let is_active = state
        .active_dirs()
        .into_iter()
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
        .any(|path| path == dir);
    if is_active {
        return api_error(
            StatusCode::CONFLICT,
            "该文件夹正在录制，请先停止录制或等待自动轮换",
        );
    }

    let settings = state.settings_snapshot();
    let fps = body.fps.unwrap_or(settings.convert_fps);
    if !fps.is_finite() || !(0.05..=120.0).contains(&fps) {
        return api_error(StatusCode::BAD_REQUEST, "fps 需在 0.05 ~ 120 之间");
    }

    match convert::enqueue(&state, dir, fps) {
        Ok(()) => Json(json!({"success": true})).into_response(),
        Err(message) => api_error(StatusCode::CONFLICT, message),
    }
}

async fn convert_all(State(state): State<Arc<AppState>>) -> Json<Value> {
    let queued = scheduler::convert_all_now(&state).await;
    Json(json!({"success": true, "queued": queued}))
}

#[derive(Deserialize)]
struct DeleteBody {
    path: String,
}

async fn delete_entry(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DeleteBody>,
) -> Response {
    let root = match std::fs::canonicalize(&state.config.recordings_dir) {
        Ok(root) => root,
        Err(err) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("录像目录不存在: {err}"),
            )
        }
    };
    let requested = body.path.trim();
    if requested.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "缺少路径");
    }

    // A `@video/...` reference points at the configurable export directory
    // (which may be outside recordings); everything else is under recordings.
    let (path, key, is_dir) = if requested.starts_with("@video/") {
        match convert::resolve_video_ref(
            &state.config.recordings_dir,
            &state.config.video_dir,
            requested,
        ) {
            Ok((path, key)) => (path, key, false),
            Err(err) => return api_error(StatusCode::BAD_REQUEST, format!("{err:#}")),
        }
    } else {
        let relative = requested.trim_start_matches('/');
        if relative.is_empty() {
            return api_error(StatusCode::BAD_REQUEST, "缺少路径");
        }
        let path = match std::fs::canonicalize(root.join(relative)) {
            Ok(path) => path,
            Err(_) => return api_error(StatusCode::NOT_FOUND, "路径不存在"),
        };
        if path == root || !path.starts_with(&root) {
            return api_error(StatusCode::BAD_REQUEST, "非法路径");
        }
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) => {
                return api_error(StatusCode::NOT_FOUND, format!("无法读取路径: {err}"))
            }
        };
        let is_dir = metadata.is_dir();
        let lowercase_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let is_video = !is_dir
            && lowercase_name.ends_with(".mp4")
            && !lowercase_name.ends_with(".orig.mp4");
        if !is_dir && !is_video {
            return api_error(StatusCode::BAD_REQUEST, "只能删除录像文件夹或 MP4 视频");
        }
        let key = convert::relative_key(&root, &path);
        (path, key, is_dir)
    };

    let active: Vec<PathBuf> = state
        .active_dirs()
        .into_iter()
        .map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir))
        .collect();
    let touches_active = active
        .iter()
        .any(|dir| dir == &path || path.starts_with(dir) || (is_dir && dir.starts_with(&path)));
    if touches_active {
        return api_error(
            StatusCode::CONFLICT,
            "该录像正在录制，请先停止录制后再删除",
        );
    }

    {
        let jobs = state.conversions.lock().unwrap();
        let busy = jobs
            .get(&key)
            .map(|job| job.state == "queued" || job.state == "running")
            .unwrap_or(false);
        let child_busy = is_dir
            && jobs.iter().any(|(job_key, job)| {
                (job.state == "queued" || job.state == "running")
                    && job_key.starts_with(&format!("{key}/"))
            });
        if busy || child_busy {
            return api_error(StatusCode::CONFLICT, "该录像正在转换，无法删除");
        }
    }

    let result = if is_dir {
        tokio::fs::remove_dir_all(&path).await
    } else {
        // Also clean up an old <name>.orig.mp4 backup, if any.
        if let (Some(parent), Some(stem)) = (path.parent(), path.file_stem()) {
            let backup = parent.join(format!("{}.orig.mp4", stem.to_string_lossy()));
            let _ = tokio::fs::remove_file(&backup).await;
        }
        tokio::fs::remove_file(&path).await
    };

    match result {
        Ok(()) => {
            state.bump_history();
            Json(json!({"success": true, "deleted": true})).into_response()
        }
        Err(err) => api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("删除失败: {err}")),
    }
}

async fn conversions(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut jobs: Vec<_> = {
        let jobs = state.conversions.lock().unwrap();
        jobs.values().cloned().collect()
    };
    jobs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Json(json!({"jobs": jobs}))
}

async fn get_settings(State(state): State<Arc<AppState>>) -> Json<Value> {
    let settings = state.settings_snapshot();
    let next = scheduler::next_run(&settings.auto_convert_time, Local::now())
        .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string());
    Json(json!({
        "auto_convert": {
            "enabled": settings.auto_convert_enabled,
            "time": settings.auto_convert_time,
            "fps": settings.convert_fps,
            "next_run": next,
        },
        "recording_interval": settings.recording_interval,
        "recordings_dir": state.config.recordings_dir.display().to_string(),
        "video_dir": state.config.video_dir.display().to_string(),
        "ffmpeg_available": state.ffmpeg_available,
    }))
}

#[derive(Deserialize)]
struct SettingsBody {
    auto_convert_enabled: Option<bool>,
    auto_convert_time: Option<String>,
    convert_fps: Option<f64>,
    recording_interval: Option<f64>,
}

async fn set_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SettingsBody>,
) -> Response {
    if let Some(text) = &body.auto_convert_time {
        if parse_hhmm(text).is_none() {
            return api_error(StatusCode::BAD_REQUEST, "时间格式应为 HH:MM");
        }
    }
    if let Some(fps) = body.convert_fps {
        if !fps.is_finite() || !(0.05..=120.0).contains(&fps) {
            return api_error(StatusCode::BAD_REQUEST, "fps 需在 0.05 ~ 120 之间");
        }
    }
    if let Some(interval) = body.recording_interval {
        if !interval.is_finite() || interval < 0.01 {
            return api_error(StatusCode::BAD_REQUEST, "录制间隔需 >= 0.01");
        }
    }

    {
        let mut settings = state.settings.lock().unwrap();
        if let Some(enabled) = body.auto_convert_enabled {
            settings.auto_convert_enabled = enabled;
        }
        if let Some(time) = body.auto_convert_time {
            settings.auto_convert_time = time;
        }
        if let Some(fps) = body.convert_fps {
            settings.convert_fps = fps;
        }
        if let Some(interval) = body.recording_interval {
            settings.recording_interval = interval;
        }
        settings.normalize();

        if let Err(err) = settings.save(&state.settings_path) {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot save settings: {err}"),
            );
        }
        state
            .interval_ms
            .store(settings.interval_ms(), Ordering::Relaxed);
    }

    state.settings_notify.notify_one();
    get_settings(State(state)).await.into_response()
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.started_at.elapsed().as_secs(),
        "camera_active": state.camera_keep_alive.load(Ordering::SeqCst),
        "motion": state.motion.load(Ordering::SeqCst),
        "clients": state.active_clients.load(Ordering::SeqCst),
        "recordings_dir": state.config.recordings_dir.display().to_string(),
        "video_dir": state.config.video_dir.display().to_string(),
        "ffmpeg_available": state.ffmpeg_available,
    }))
}

fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({"success": false, "error": message.into()})),
    )
        .into_response()
}
