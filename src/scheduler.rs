use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Days, Local, NaiveDateTime, TimeZone};
use tracing::{info, warn};

use crate::convert;
use crate::recording;
use crate::state::{parse_hhmm, AppState};

/// Sleep until the configured local time of day, then rotate active recordings
/// and queue every closed image folder for MP4 conversion.
pub async fn run(state: Arc<AppState>) {
    loop {
        let settings = state.settings_snapshot();
        if !settings.auto_convert_enabled {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(60)) => {},
                _ = state.settings_notify.notified() => {},
            }
            continue;
        }

        let next = match next_run(&settings.auto_convert_time, Local::now()) {
            Some(next) => next,
            None => {
                warn!("invalid auto-convert time {:?}", settings.auto_convert_time);
                tokio::time::sleep(Duration::from_secs(60)).await;
                continue;
            }
        };

        let wait = (next - Local::now()).to_std().unwrap_or(Duration::from_secs(60));
        info!(
            "next automatic conversion: {}",
            next.format("%Y-%m-%d %H:%M:%S")
        );

        tokio::select! {
            _ = tokio::time::sleep(wait) => {
                run_auto_convert(Arc::clone(&state)).await;
            }
            _ = state.settings_notify.notified() => {
                // Settings changed: recompute the schedule from scratch.
            }
        }
    }
}

pub fn next_run(time_text: &str, now: DateTime<Local>) -> Option<DateTime<Local>> {
    let time = parse_hhmm(time_text)?;
    let today = now.date_naive().and_time(time);
    let candidate = resolve_local(today)?;
    if candidate > now {
        return Some(candidate);
    }
    let tomorrow = (now.date_naive() + Days::new(1)).and_time(time);
    resolve_local(tomorrow)
}

fn resolve_local(naive: NaiveDateTime) -> Option<DateTime<Local>> {
    Local.from_local_datetime(&naive).earliest()
}

async fn run_auto_convert(state: Arc<AppState>) {
    let settings = state.settings_snapshot();
    info!(
        "automatic conversion started (fps {})",
        settings.convert_fps
    );

    // Stop writing to the current folders, then immediately continue recording
    // into fresh ones. Only closed folders are handed to the converter.
    match recording::rotate_interval(&state) {
        Ok(Some((old, new))) => info!(
            "closed interval folder {} (new: {})",
            old.display(),
            new.display()
        ),
        Ok(None) => {}
        Err(err) => warn!("cannot rotate interval recording: {err}"),
    }
    match recording::rotate_motion(&state) {
        Ok(Some((old, new))) => info!(
            "closed motion folder {} (new: {})",
            old.display(),
            new.display()
        ),
        Ok(None) => {}
        Err(err) => warn!("cannot rotate motion recording: {err}"),
    }

    let scan_state = Arc::clone(&state);
    let pending = match tokio::task::spawn_blocking(move || convert::pending_folders(&scan_state)).await
    {
        Ok(pending) => pending,
        Err(err) => {
            warn!("cannot scan folders for conversion: {err}");
            return;
        }
    };

    let mut queued = 0usize;
    for dir in pending {
        match convert::enqueue(&state, dir, settings.convert_fps) {
            Ok(()) => queued += 1,
            Err(err) => info!("conversion skipped: {err}"),
        }
    }
    info!("automatic conversion queued {queued} folder(s)");
}

/// Queue all closed image folders now, without rotating active recordings.
pub async fn convert_all_now(state: &Arc<AppState>) -> usize {
    let settings = state.settings_snapshot();
    let scan_state = Arc::clone(state);
    let pending = match tokio::task::spawn_blocking(move || convert::pending_folders(&scan_state)).await
    {
        Ok(pending) => pending,
        Err(_) => return 0,
    };

    let mut queued = 0usize;
    for dir in pending {
        if convert::enqueue(state, dir, settings.convert_fps).is_ok() {
            queued += 1;
        }
    }
    queued
}
