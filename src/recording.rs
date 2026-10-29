use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Local;
use tracing::{info, warn};

use crate::state::AppState;

/// Circular image counters: 172800 = 10 s interval * 24 h * 20 days.
pub const MAX_RECORDING_IMAGES: u64 = 172_800;
pub const MAX_MOTION_RECORDING_IMAGES: u64 = 172_800;

pub fn spawn_recorders(state: Arc<AppState>) {
    let interval_state = Arc::clone(&state);
    let _ = std::thread::Builder::new()
        .name("recording".to_string())
        .spawn(move || interval_loop(interval_state));

    let motion_state = Arc::clone(&state);
    let _ = std::thread::Builder::new()
        .name("motion-recording".to_string())
        .spawn(move || motion_loop(motion_state));
}

fn create_session_dir(base: &Path, prefix: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(base)?;
    let stamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
    for attempt in 0..1000 {
        let name = if attempt == 0 {
            format!("{prefix}{stamp}")
        } else {
            format!("{prefix}{stamp}_{attempt}")
        };
        let path = base.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "cannot allocate a unique recording folder",
    ))
}

pub fn start_interval(state: &Arc<AppState>) -> io::Result<PathBuf> {
    let mut rec = state.rec.lock().unwrap();
    if rec.active {
        if let Some(dir) = &rec.dir {
            return Ok(dir.clone());
        }
    }
    let dir = create_session_dir(&state.config.recordings_dir, "")?;
    rec.active = true;
    rec.dir = Some(dir.clone());
    rec.counter = 0;
    rec.last_saved = None;
    drop(rec);
    state.bump_history();
    info!("interval recording started in {}", dir.display());
    Ok(dir)
}

pub fn stop_interval(state: &Arc<AppState>) -> Option<PathBuf> {
    let mut rec = state.rec.lock().unwrap();
    let dir = rec.dir.clone();
    if rec.active {
        rec.active = false;
        info!("interval recording stopped ({})", dir.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
    }
    state.bump_history();
    dir
}

/// Close the current folder and start a fresh one without interrupting recording.
pub fn rotate_interval(state: &Arc<AppState>) -> io::Result<Option<(PathBuf, PathBuf)>> {
    let mut rec = state.rec.lock().unwrap();
    if !rec.active {
        return Ok(None);
    }
    let old = rec.dir.clone();
    let new = create_session_dir(&state.config.recordings_dir, "")?;
    rec.dir = Some(new.clone());
    rec.counter = 0;
    rec.last_saved = None;
    drop(rec);
    state.bump_history();
    match old {
        Some(old) => {
            info!("interval recording rotated: {} -> {}", old.display(), new.display());
            Ok(Some((old, new)))
        }
        None => Ok(None),
    }
}

pub fn start_motion(state: &Arc<AppState>) -> io::Result<PathBuf> {
    let mut rec = state.motion_rec.lock().unwrap();
    if rec.active {
        if let Some(dir) = &rec.dir {
            return Ok(dir.clone());
        }
    }
    let dir = create_session_dir(&state.config.recordings_dir, "motion_")?;
    rec.active = true;
    rec.dir = Some(dir.clone());
    rec.counter = 0;
    rec.last_saved = None;
    rec.last_seq = None;
    drop(rec);
    state.bump_history();
    info!("motion recording started in {}", dir.display());
    Ok(dir)
}

pub fn stop_motion(state: &Arc<AppState>) -> Option<PathBuf> {
    let mut rec = state.motion_rec.lock().unwrap();
    let dir = rec.dir.clone();
    if rec.active {
        rec.active = false;
        info!("motion recording stopped ({})", dir.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
    }
    state.bump_history();
    dir
}

pub fn rotate_motion(state: &Arc<AppState>) -> io::Result<Option<(PathBuf, PathBuf)>> {
    let mut rec = state.motion_rec.lock().unwrap();
    if !rec.active {
        return Ok(None);
    }
    let old = rec.dir.clone();
    let new = create_session_dir(&state.config.recordings_dir, "motion_")?;
    rec.dir = Some(new.clone());
    rec.counter = 0;
    rec.last_saved = None;
    rec.last_seq = None;
    drop(rec);
    state.bump_history();
    match old {
        Some(old) => {
            info!("motion recording rotated: {} -> {}", old.display(), new.display());
            Ok(Some((old, new)))
        }
        None => Ok(None),
    }
}

fn interval_loop(state: Arc<AppState>) {
    loop {
        let active = state.rec.lock().unwrap().active;
        if !active {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        let interval = Duration::from_millis(state.interval_ms.load(Ordering::Relaxed).max(10));
        let frame = match state.latest_frame() {
            Some(frame) => frame,
            None => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };

        {
            // Held across the write so rotation cannot race a late snapshot.
            let mut rec = state.rec.lock().unwrap();
            if !rec.active {
                continue;
            }
            let due = rec
                .last_saved
                .map(|last| last.elapsed() >= interval)
                .unwrap_or(true);
            if due {
                if let Some(dir) = rec.dir.clone() {
                    let name = format!("img-{:06}.jpg", rec.counter);
                    rec.counter = (rec.counter + 1) % MAX_RECORDING_IMAGES;
                    match std::fs::write(dir.join(name), &frame.jpeg) {
                        Ok(()) => rec.last_saved = Some(Instant::now()),
                        Err(err) => {
                            warn!("cannot save snapshot: {err}");
                            rec.last_saved = Some(Instant::now());
                        }
                    }
                }
            }
        }

        let sleep_ms = (interval.as_millis() as u64 / 2).clamp(5, 50);
        std::thread::sleep(Duration::from_millis(sleep_ms));
    }
}

fn motion_loop(state: Arc<AppState>) {
    loop {
        let active = state.motion_rec.lock().unwrap().active;
        if !active {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if !state.motion.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }

        let frame = match state.latest_frame() {
            Some(frame) => frame,
            None => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };

        {
            let mut rec = state.motion_rec.lock().unwrap();
            if !rec.active || rec.last_seq == Some(frame.seq) {
                continue;
            }
            if let Some(dir) = rec.dir.clone() {
                let name = format!("motion-{:06}.jpg", rec.counter);
                rec.counter = (rec.counter + 1) % MAX_MOTION_RECORDING_IMAGES;
                match std::fs::write(dir.join(name), &frame.jpeg) {
                    Ok(()) => rec.last_seq = Some(frame.seq),
                    Err(err) => {
                        warn!("cannot save motion frame: {err}");
                        rec.last_seq = Some(frame.seq);
                    }
                }
            }
        }

        // Match the capture cadence; newer frames overwrite the shared slot.
        std::thread::sleep(Duration::from_millis(10));
    }
}
