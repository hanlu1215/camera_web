# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

Rust rewrite of a webcam monitor (the old Python implementation remains in `camera_web.py` as a reference only). `camera_web` serves an MJPEG live view, runs motion detection, records JPEG snapshots (interval and motion-triggered), converts image folders to MP4, and schedules a daily conversion that rotates active recordings. The entire UI is one embedded file: `src/ui.html` (included with `include_str!`).

## Commands

```bash
cargo build --release          # build
./target/release/camera_web    # run on 0.0.0.0:5000, recordings in ./recordings
cargo check                    # fast type check
```

There is no test suite or lint config. ffmpeg (and ideally ffprobe) must be on PATH at runtime — it is used both to capture `/dev/videoN` and to encode/verify MP4 conversions.

Development without a camera (camera access is not required):

```bash
./target/release/camera_web --test-pattern --bind 127.0.0.1 --port 5010 \
  --recordings-dir /tmp/camrec --settings /tmp/camrec/settings.json --width 640 --height 360 --fps 15
```

Then exercise the API with `curl` (see README for the endpoint table). The synthetic `testsrc2` pattern triggers motion detection, so recording and conversion can be tested end-to-end.

## Architecture

```
main.rs       CLI parsing, AppState construction, spawns worker threads/tasks
config.rs     clap CLI flags + camera_web_config.json -> RuntimeConfig
state.rs      AppState, Frame, RecorderState, persisted Settings, camera lifecycle helpers
camera.rs     ffmpeg capture thread: raw RGB -> motion detection -> watermark -> JPEG
recording.rs  interval + motion recorder threads, session folder creation and rotation
convert.rs    ffmpeg conversion queue/worker, concat-list generation, verify-then-delete
history.rs    recordings directory scan (cached) for the history UI/API
scheduler.rs  daily auto-convert scheduling and rotation
web.rs        axum router, MJPEG stream, JSON API, legacy endpoints, /media serving
ui.html       single-page UI (vanilla JS)
```

Concurrency model:

- **One camera thread** (lazily spawned, never exits while the process lives) runs an ffmpeg child process that emits `rawvideo rgb24` frames. It owns motion detection, drawing, JPEG encoding, and publishing the latest `Arc<Frame>` into `AppState.frame`. The JPEG is encoded once and shared by every MJPEG client and recorder (`Frame.jpeg` is `bytes::Bytes`, so clones are cheap).
- **Two recorder threads** (`interval_loop`, `motion_loop`) poll the shared frame and write it to disk. They hold the relevant recorder mutex across the file write so rotation cannot let a late image land in an already-closed folder.
- **A conversion worker** is a tokio task consuming an unbounded channel; conversions are strictly serial. `AppState.conversions` tracks queued/running/done/error status by path (relative to recordings, or `@video/<rel>` for exported videos).
- `config.rs` resolves CLI flags plus an optional `camera_web_config.json` (`recordings_dir`, `video_dir`, `settings`) into `RuntimeConfig`. The config file is the intended single place to change storage paths; CLI flags override it. If the file is missing, an example is written. `video_dir` may live outside `recordings_dir`.
- **The scheduler** is a tokio task that sleeps until the next local HH:MM. `settings_notify` (a `tokio::sync::Notify`, used with `notify_one`) interrupts the sleep when settings change.
- **HTTP handlers** use `std::sync::Mutex` only for short critical sections (never across `.await`).

Camera lifecycle invariants (preserve these when touching routes):

- The camera is opened lazily on the first `/video_feed` request or recording start (`ensure_camera_started`).
- `active_clients` is incremented/decremented under `client_lock`; the camera is released (`camera_keep_alive = false`) only when there are no clients **and** neither recorder is active (`maybe_release_camera`). The camera thread then kills ffmpeg and clears the frame.
- Recorders read from `output_frame`, not the camera, so they require the camera thread to be running; starting a recording always calls `ensure_camera_started`.

Capture details:

- `camera.rs` spawns `ffmpeg -f v4l2 [-framerate N -video_size WxH] -i /dev/videoN -vf scale=W:H -pix_fmt rgb24 -f rawvideo -`.
- If the first process exits before producing a frame, it retries once without explicit size/framerate (some cameras reject them) and then keeps retrying with backoff. ffmpeg stderr is drained by a logging thread.
- Frames are read with `read_exact` into a reusable buffer; the buffer is handed to `RgbImage::from_raw` and recovered with `into_raw` each iteration to avoid per-frame allocation.
- Motion detection downsizes to `--motion-width`, grayscales, blurs, thresholds `abs_diff > motion_threshold`, dilates twice, then does 4-connected-component analysis; components with area ≥ `motion_min_area` become boxes scaled back to full resolution.
- The timestamp watermark is drawn with the `font8x8` bitmap font; motion boxes are red outlines. Watermark/box drawing happens in `camera.rs` only (unlike the Python version).

Conversion invariants:

- `convert_folder` collects only `.jpg`/`.jpeg` files in the folder, sorts by mtime (the 6-digit counter wraps, so names are not chronological), and writes a `ffconcat` list with `duration = 1/fps` per frame.
- The output goes to `<video_dir>/<folder>.mp4` (default `<recordings>/Video`; not inside the source folder). It encodes a hidden `<video_dir>/.<folder>.part` temp, verifies with ffprobe (if available) plus a minimum size check, renames to `<folder>.mp4`, and only then **recursively deletes the source folder**. A failed conversion leaves the folder untouched.
- `history.rs` skips `video_dir` during the recordings scan and adds a synthetic `@video` group instead, so the export directory is listed once whether it is inside recordings or on another disk. Videos from `video_dir` use API paths `@video/<rel>` and URLs `/media-video/<rel>`; recording-internal videos use plain relative paths and `/media/<rel>`.
- Paths from the API go through `resolve_recording_dir` / `resolve_video_ref`, which canonicalize and reject anything outside the relevant root (recordings or `video_dir`). Delete accepts `@video/<rel>` for exported videos and plain paths for recording items.
- Active recording folders are rejected for manual conversion/deletion (`409`) and skipped by `pending_folders`. Delete also rejects entries with queued/running conversion jobs.

Auto-convert semantics (`scheduler.rs`):

- Disabled by default. When enabled, it runs once per day at the local time in settings.
- On run: rotate active interval/motion recordings (old folder closed, new folder created, recording continues), then enqueue every closed folder that still contains JPGs for conversion at `convert_fps`.
- The setting is persisted to the JSON file given by `--settings`; `Settings::load` tolerates a missing/corrupt file and `normalize` repairs out-of-range values.

Web/API notes:

- `/video_feed` streams multipart JPEG frames and re-sends only when the frame sequence changes; a dropped client releases the camera through `ClientGuard`.
- `/media` and `/media-video` are `tower-http` `ServeDir`s over `recordings_dir` and `video_dir`, so MP4 Range requests (seeking) work even when the export dir is on another disk.
- Legacy Python endpoints under `/status`, `/start_recording`, etc. are kept and share handlers with `/api/...`.
- The UI polls `/api/status` (800 ms), `/api/recording/status` (1 s), `/api/motion_recording_status` (500 ms), and `/api/history` (12 s). Keep JS field names and JSON response keys in sync.

## Deployment

- CI: `.github/workflows/rust-build.yml` builds on Ubuntu 22.04 for `v*.*.*` tags (release) or manual dispatch (artifact). The binary is dynamically linked and needs glibc ≥ 2.35 plus ffmpeg on the target.
- The running service is `camera-web.service`, executing `/vol1/1000/TEMP/camera_web_bin` with `WorkingDirectory=/vol1/1000/TEMP` and recordings in `/vol1/1000/TEMP/recordings`. `deploy/camera-web.service` and `deploy/install-service.sh` mirror that deployment; the systemd unit and restart require root, while the binary path itself lives in a user-writable directory.
- The unit passes `--start-motion-recording`, so motion recording resumes as a new folder after every service restart. Recorder state is otherwise in-memory only (not persisted).
- Recordings and settings are created as the service user (currently `root`), so files under `/vol1/1000/TEMP/recordings` may not be readable by the normal login user.

## Style

- Chinese user-facing labels/comments, English identifiers; 4-space indent; no type annotations beyond Rust's own.
- Keep the UI dependency-free (no external CDN assets) — it must work on a LAN without internet access.
