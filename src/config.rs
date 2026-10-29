use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Deserialize;
use tracing::warn;

/// Optional startup configuration file. Paths can also be overridden on the
/// command line (CLI wins), but the file is intended to be the single place
/// to change where recordings and exported videos are stored.
#[derive(Clone, Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    recordings_dir: Option<PathBuf>,
    #[serde(default)]
    video_dir: Option<PathBuf>,
    #[serde(default)]
    settings: Option<PathBuf>,
}

#[derive(Parser, Clone, Debug)]
#[command(
    name = "camera_web",
    version,
    about = "Camera monitor: MJPEG live view, motion detection, snapshot recording and MP4 archiving"
)]
pub struct Config {
    /// Startup config file (paths only)
    #[arg(long, default_value = "./camera_web_config.json")]
    pub config: PathBuf,

    /// Camera device index (0 = /dev/video0)
    #[arg(long, default_value_t = 0)]
    pub camera: u32,

    /// Requested capture width
    #[arg(long, default_value_t = 1280)]
    pub width: u32,

    /// Requested capture height
    #[arg(long, default_value_t = 720)]
    pub height: u32,

    /// Requested capture frames per second
    #[arg(long, default_value_t = 30)]
    pub fps: u32,

    /// HTTP listen address
    #[arg(long, default_value = "0.0.0.0")]
    pub bind: String,

    /// HTTP listen port
    #[arg(long, default_value_t = 5000)]
    pub port: u16,

    /// Root directory for recording folders (overrides the config file)
    #[arg(long)]
    pub recordings_dir: Option<PathBuf>,

    /// Directory for converted MP4s (overrides the config file)
    #[arg(long)]
    pub video_dir: Option<PathBuf>,

    /// Path of the persisted runtime settings JSON (overrides the config file)
    #[arg(long)]
    pub settings: Option<PathBuf>,

    /// ffmpeg executable used for capture and conversion
    #[arg(long, default_value = "ffmpeg")]
    pub ffmpeg: String,

    /// ffmpeg encoder threads (keep low so live capture is not starved)
    #[arg(long, default_value_t = 2)]
    pub ffmpeg_threads: u32,

    /// JPEG quality of the live/recorded frames (1-100)
    #[arg(long, default_value_t = 80)]
    pub jpeg_quality: u8,

    /// Per-pixel difference threshold for motion detection (0-255)
    #[arg(long, default_value_t = 20)]
    pub motion_threshold: u8,

    /// Minimum changed-pixel area (in the downscaled detection image)
    #[arg(long, default_value_t = 50)]
    pub motion_min_area: u32,

    /// Width of the downscaled image used for motion detection
    #[arg(long, default_value_t = 320)]
    pub motion_width: u32,

    /// Seconds between attempts to (re)open a missing camera
    #[arg(long, default_value_t = 3.0)]
    pub camera_retry_secs: f64,

    /// Start interval recording as soon as the service starts
    #[arg(long)]
    pub start_recording: bool,

    /// Start motion-triggered recording as soon as the service starts
    #[arg(long)]
    pub start_motion_recording: bool,

    /// Use ffmpeg's synthetic test pattern instead of a camera (for testing)
    #[arg(long, hide = true)]
    pub test_pattern: bool,
}

/// Fully resolved configuration used by the rest of the program.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub camera: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bind: String,
    pub port: u16,
    pub recordings_dir: PathBuf,
    pub video_dir: PathBuf,
    pub settings: PathBuf,
    pub ffmpeg: String,
    pub ffmpeg_threads: u32,
    pub jpeg_quality: u8,
    pub motion_threshold: u8,
    pub motion_min_area: u32,
    pub motion_width: u32,
    pub camera_retry_secs: f64,
    pub start_recording: bool,
    pub start_motion_recording: bool,
    pub test_pattern: bool,
}

impl Config {
    /// Load the config file (if present) and resolve the folder paths.
    /// CLI flags take precedence over the file; the file takes precedence over
    /// built-in defaults. If the file does not exist, an example is written.
    pub fn resolve(self) -> RuntimeConfig {
        let file = self.load_file();

        let recordings_dir = self
            .recordings_dir
            .clone()
            .or(file.recordings_dir)
            .unwrap_or_else(|| PathBuf::from("./recordings"));
        let video_dir = self
            .video_dir
            .clone()
            .or(file.video_dir)
            .unwrap_or_else(|| recordings_dir.join("Video"));
        let settings = self
            .settings
            .clone()
            .or(file.settings)
            .unwrap_or_else(|| PathBuf::from("./camera_web_settings.json"));

        self.write_example_if_missing(&recordings_dir, &video_dir, &settings);

        RuntimeConfig {
            camera: self.camera,
            width: self.width,
            height: self.height,
            fps: self.fps,
            bind: self.bind,
            port: self.port,
            recordings_dir,
            video_dir,
            settings,
            ffmpeg: self.ffmpeg,
            ffmpeg_threads: self.ffmpeg_threads,
            jpeg_quality: self.jpeg_quality,
            motion_threshold: self.motion_threshold,
            motion_min_area: self.motion_min_area,
            motion_width: self.motion_width,
            camera_retry_secs: self.camera_retry_secs,
            start_recording: self.start_recording,
            start_motion_recording: self.start_motion_recording,
            test_pattern: self.test_pattern,
        }
    }

    fn load_file(&self) -> FileConfig {
        let text = match std::fs::read_to_string(&self.config) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return FileConfig::default();
            }
            Err(err) => {
                warn!("cannot read config {}: {err}", self.config.display());
                return FileConfig::default();
            }
        };
        match serde_json::from_str(&text) {
            Ok(file) => file,
            Err(err) => {
                warn!(
                    "cannot parse config {}: {err}; using defaults",
                    self.config.display()
                );
                FileConfig::default()
            }
        }
    }

    fn write_example_if_missing(&self, recordings: &Path, video: &Path, settings: &Path) {
        if self.config.exists() {
            return;
        }
        let example = serde_json::json!({
            "_comment": "改动这里的路径后重启 camera-web 即可。相对路径相对于启动时的工作目录。video_dir 可以放在 recordings_dir 之外。",
            "recordings_dir": recordings.display().to_string(),
            "video_dir": video.display().to_string(),
            "settings": settings.display().to_string(),
        });
        let text = match serde_json::to_string_pretty(&example) {
            Ok(text) => text,
            Err(_) => return,
        };
        if let Err(err) = std::fs::write(&self.config, text) {
            warn!(
                "cannot write example config {}: {err}",
                self.config.display()
            );
        }
    }
}
