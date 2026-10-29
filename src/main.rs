mod camera;
mod config;
mod convert;
mod history;
mod recording;
mod scheduler;
mod state;
mod web;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use clap::Parser;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::Config;
use crate::state::{AppState, Settings};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::parse().resolve();
    std::fs::create_dir_all(&config.recordings_dir)?;
    std::fs::create_dir_all(&config.video_dir)?;

    let settings = Settings::load(&config.settings);
    if !config.settings.exists() {
        if let Err(err) = settings.save(&config.settings) {
            warn!("cannot write default settings {}: {err}", config.settings.display());
        }
    }
    let interval_ms = settings.interval_ms();
    let settings_path = config.settings.clone();
    let start_recording = config.start_recording;
    let start_motion_recording = config.start_motion_recording;

    let (convert_tx, convert_rx) = mpsc::unbounded_channel();
    let ffmpeg_available = convert::ffmpeg_available(&config.ffmpeg);
    if !ffmpeg_available {
        warn!(
            "ffmpeg not found at {:?}; image -> mp4 conversion will fail until it is installed",
            config.ffmpeg
        );
    }

    let state = Arc::new(AppState {
        config,
        settings: Mutex::new(settings),
        settings_path,
        settings_notify: tokio::sync::Notify::new(),
        frame: Mutex::new(None),
        motion: AtomicBool::new(false),
        active_clients: AtomicUsize::new(0),
        client_lock: Mutex::new(()),
        camera_keep_alive: AtomicBool::new(false),
        camera_thread_started: Mutex::new(false),
        interval_ms: AtomicU64::new(interval_ms),
        rec: Mutex::new(Default::default()),
        motion_rec: Mutex::new(Default::default()),
        conversions: Mutex::new(Default::default()),
        convert_tx,
        history_cache: Mutex::new(None),
        ffmpeg_available,
        started_at: Instant::now(),
    });

    tokio::spawn(convert::convert_worker(Arc::clone(&state), convert_rx));
    recording::spawn_recorders(Arc::clone(&state));
    tokio::spawn(scheduler::run(Arc::clone(&state)));

    // Optional boot behaviour: keep recording across service restarts.
    if start_recording {
        state.ensure_camera_started();
        if let Err(err) = recording::start_interval(&state) {
            warn!("cannot start interval recording at boot: {err}");
        }
    }
    if start_motion_recording {
        state.ensure_camera_started();
        if let Err(err) = recording::start_motion(&state) {
            warn!("cannot start motion recording at boot: {err}");
        }
    }

    let app = web::router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind((state.config.bind.as_str(), state.config.port)).await?;
    info!(
        "camera_web v{} listening on http://{}:{} (recordings: {}, videos: {})",
        env!("CARGO_PKG_VERSION"),
        state.config.bind,
        state.config.port,
        state.config.recordings_dir.display(),
        state.config.video_dir.display()
    );
    axum::serve(listener, app).await?;
    Ok(())
}
