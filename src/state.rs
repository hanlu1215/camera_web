use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use bytes::Bytes;
use chrono::NaiveTime;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify};

use crate::config::RuntimeConfig;
use crate::convert::ConvertRequest;
use crate::history::FolderInfo;

pub struct Frame {
    pub seq: u64,
    pub jpeg: Bytes,
    pub captured_at: SystemTime,
}

#[derive(Default)]
pub struct RecorderState {
    pub active: bool,
    pub dir: Option<PathBuf>,
    pub counter: u64,
    pub last_saved: Option<Instant>,
    /// Last frame sequence written by the motion recorder.
    pub last_seq: Option<u64>,
}

fn default_auto_enabled() -> bool {
    false
}
fn default_auto_time() -> String {
    "03:00".to_string()
}
fn default_convert_fps() -> f64 {
    30.0
}
fn default_interval() -> f64 {
    10.0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_auto_enabled")]
    pub auto_convert_enabled: bool,
    /// Local time of day in HH:MM
    #[serde(default = "default_auto_time")]
    pub auto_convert_time: String,
    #[serde(default = "default_convert_fps")]
    pub convert_fps: f64,
    /// Snapshot recording interval in seconds
    #[serde(default = "default_interval")]
    pub recording_interval: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_convert_enabled: default_auto_enabled(),
            auto_convert_time: default_auto_time(),
            convert_fps: default_convert_fps(),
            recording_interval: default_interval(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
                Ok(mut settings) => {
                    settings.normalize();
                    settings
                }
                Err(err) => {
                    tracing::warn!("cannot parse settings {}: {err}", path.display());
                    Settings::default()
                }
            },
            Err(_) => Settings::default(),
        }
    }

    pub fn normalize(&mut self) {
        if self.recording_interval < 0.01 || !self.recording_interval.is_finite() {
            self.recording_interval = default_interval();
        }
        if self.convert_fps < 1.0 || !self.convert_fps.is_finite() {
            self.convert_fps = default_convert_fps();
        }
        if parse_hhmm(&self.auto_convert_time).is_none() {
            self.auto_convert_time = default_auto_time();
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    pub fn interval_ms(&self) -> u64 {
        (self.recording_interval.max(0.01) * 1000.0).round() as u64
    }
}

pub fn parse_hhmm(text: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(text.trim(), "%H:%M").ok()
}

#[derive(Clone, Debug, Serialize)]
pub struct ConvertJobStatus {
    pub path: String,
    /// queued | running | done | error
    pub state: String,
    pub message: String,
    pub output: String,
    pub images: usize,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

impl ConvertJobStatus {
    pub fn new(path: String) -> Self {
        Self {
            path,
            state: "queued".to_string(),
            message: String::new(),
            output: String::new(),
            images: 0,
            started_at: None,
            finished_at: None,
        }
    }
}

pub struct AppState {
    pub config: RuntimeConfig,
    pub settings: Mutex<Settings>,
    pub settings_path: PathBuf,
    pub settings_notify: Notify,
    pub frame: Mutex<Option<Arc<Frame>>>,
    pub motion: AtomicBool,
    pub active_clients: AtomicUsize,
    /// Serializes client attach/detach with the camera-release decision.
    pub client_lock: Mutex<()>,
    pub camera_keep_alive: AtomicBool,
    pub camera_thread_started: Mutex<bool>,
    pub interval_ms: AtomicU64,
    pub rec: Mutex<RecorderState>,
    pub motion_rec: Mutex<RecorderState>,
    pub conversions: Mutex<HashMap<String, ConvertJobStatus>>,
    pub convert_tx: mpsc::UnboundedSender<ConvertRequest>,
    pub history_cache: Mutex<Option<(Instant, Vec<FolderInfo>)>>,
    pub ffmpeg_available: bool,
    pub started_at: Instant,
}

impl AppState {
    pub fn latest_frame(&self) -> Option<Arc<Frame>> {
        self.frame.lock().unwrap().clone()
    }

    /// Mark the camera as needed and lazily start the capture thread.
    pub fn ensure_camera_started(self: &Arc<Self>) {
        self.camera_keep_alive.store(true, Ordering::SeqCst);
        let mut started = self.camera_thread_started.lock().unwrap();
        if *started {
            return;
        }
        *started = true;
        let state = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("camera".to_string())
            .spawn(move || crate::camera::camera_loop(state));
        if spawned.is_err() {
            *started = false;
            tracing::error!("failed to spawn camera thread");
        }
    }

    pub fn recording_active(&self) -> bool {
        self.rec.lock().unwrap().active
    }

    pub fn motion_recording_active(&self) -> bool {
        self.motion_rec.lock().unwrap().active
    }

    /// Release the camera when nothing needs it any more.
    pub fn maybe_release_camera(&self) {
        let clients = self.active_clients.load(Ordering::SeqCst);
        if clients == 0 && !self.recording_active() && !self.motion_recording_active() {
            self.camera_keep_alive.store(false, Ordering::SeqCst);
        }
    }

    pub fn settings_snapshot(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn bump_history(&self) {
        *self.history_cache.lock().unwrap() = None;
    }

    pub fn active_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        {
            let rec = self.rec.lock().unwrap();
            if rec.active {
                if let Some(dir) = &rec.dir {
                    dirs.push(dir.clone());
                }
            }
        }
        {
            let rec = self.motion_rec.lock().unwrap();
            if rec.active {
                if let Some(dir) = &rec.dir {
                    dirs.push(dir.clone());
                }
            }
        }
        dirs
    }
}
