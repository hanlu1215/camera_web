use std::io::{BufRead, BufReader, Read};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use chrono::Local;
use font8x8::{UnicodeFonts, BASIC_FONTS};
use image::{imageops, ExtendedColorType, ImageEncoder, Rgb, RgbImage};
use tracing::{info, warn};

use crate::config::RuntimeConfig;
use crate::state::{AppState, Frame};

#[derive(Clone, Copy, Debug)]
pub struct MotionBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Frames are pulled from a long-running ffmpeg process. This keeps the
/// program free of native camera/OpenCV build dependencies; ffmpeg is already
/// required for MP4 conversion.
struct Capture {
    child: Child,
    stdout: ChildStdout,
    stderr_thread: Option<JoinHandle<()>>,
}

impl Capture {
    fn spawn(config: &RuntimeConfig, negotiate: bool) -> std::io::Result<Self> {
        let device = format!("/dev/video{}", config.camera);
        let mut command = Command::new(&config.ffmpeg);
        command
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-nostdin");
        if config.test_pattern {
            command
                .arg("-f")
                .arg("lavfi")
                .arg("-i")
                .arg(format!(
                    "testsrc2=size={}x{}:rate={}",
                    config.width,
                    config.height,
                    config.fps.max(1)
                ));
        } else {
            command.arg("-f").arg("v4l2");
            if negotiate {
                command
                    .arg("-framerate")
                    .arg(config.fps.max(1).to_string())
                    .arg("-video_size")
                    .arg(format!("{}x{}", config.width, config.height));
            }
            command.arg("-i").arg(device);
        }
        command
            .arg("-vf")
            .arg(format!("scale={}:{}", config.width, config.height))
            .arg("-pix_fmt")
            .arg("rgb24")
            .arg("-an")
            .arg("-f")
            .arg("rawvideo")
            .arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr_thread = child.stderr.take().map(|stderr| {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    let line = line.trim();
                    if !line.is_empty() {
                        warn!("camera ffmpeg: {line}");
                    }
                }
            })
        });
        Ok(Self {
            child,
            stdout,
            stderr_thread,
        })
    }

    fn close(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }
}

pub fn camera_loop(state: Arc<AppState>) {
    let config = &state.config;
    let frame_bytes = config.width as usize * config.height as usize * 3;
    if frame_bytes == 0 {
        warn!("invalid capture size {}x{}", config.width, config.height);
        return;
    }
    let target = Duration::from_secs_f64(1.0 / config.fps.max(1) as f64);

    let mut buffer = vec![0u8; frame_bytes];
    let mut prev_gray: Option<Vec<u8>> = None;
    let mut seq: u64 = 0;
    let mut frames_in_process: u64 = 0;
    let mut negotiate = true;
    let mut capture: Option<Capture> = None;

    loop {
        // ── idle: stop ffmpeg and release the camera ────────────────────────
        if !state.camera_keep_alive.load(Ordering::SeqCst) {
            if let Some(process) = capture.take() {
                process.close();
                prev_gray = None;
                *state.frame.lock().unwrap() = None;
                state.motion.store(false, Ordering::SeqCst);
                info!("camera released (no clients, no recording)");
            }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }

        // ── (re)start the capture process ───────────────────────────────────
        if capture.is_none() {
            match Capture::spawn(config, negotiate) {
                Ok(process) => {
                    info!(
                        "camera {} capture started through ffmpeg ({}x{} @ up to {}fps)",
                        config.camera, config.width, config.height, config.fps
                    );
                    capture = Some(process);
                    prev_gray = None;
                    frames_in_process = 0;
                }
                Err(err) => {
                    warn!("cannot start camera capture via ffmpeg: {err}");
                    std::thread::sleep(Duration::from_secs_f64(
                        config.camera_retry_secs.max(0.5),
                    ));
                    continue;
                }
            }
        }

        let started = Instant::now();
        let process = match capture.as_mut() {
            Some(process) => process,
            None => continue,
        };

        match process.stdout.read_exact(&mut buffer) {
            Ok(()) => {}
            Err(err) => {
                warn!("camera capture stopped: {err}");
                let had_frames = frames_in_process > 0;
                if let Some(process) = capture.take() {
                    process.close();
                }
                prev_gray = None;
                *state.frame.lock().unwrap() = None;
                state.motion.store(false, Ordering::SeqCst);
                if !had_frames && negotiate {
                    // Some cameras reject an explicit size/framerate; let the
                    // device pick its defaults and scale afterwards.
                    warn!("retrying camera with device-default format");
                    negotiate = false;
                    continue;
                }
                std::thread::sleep(Duration::from_secs_f64(
                    config.camera_retry_secs.max(0.5),
                ));
                continue;
            }
        }

        frames_in_process += 1;
        let mut rgb = match RgbImage::from_raw(config.width, config.height, std::mem::take(&mut buffer)) {
            Some(image) => image,
            None => {
                buffer = vec![0u8; frame_bytes];
                continue;
            }
        };

        let boxes = detect_motion(&state, &rgb, &mut prev_gray);
        state.motion.store(!boxes.is_empty(), Ordering::SeqCst);
        let thickness = ((config.height / 360).clamp(1, 3)).max(1);
        for b in &boxes {
            draw_outline(&mut rgb, b, thickness, Rgb([255, 0, 0]));
        }
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        draw_watermark(&mut rgb, &timestamp);

        match encode_jpeg(&rgb, config.jpeg_quality) {
            Ok(jpeg) => {
                let frame = Arc::new(Frame {
                    seq,
                    jpeg: bytes::Bytes::from(jpeg),
                    captured_at: SystemTime::now(),
                });
                seq = seq.wrapping_add(1);
                *state.frame.lock().unwrap() = Some(frame);
            }
            Err(err) => warn!("jpeg encode failed: {err}"),
        }
        buffer = rgb.into_raw();

        if let Some(remaining) = target.checked_sub(started.elapsed()) {
            std::thread::sleep(remaining);
        }
    }
}

fn encode_jpeg(image: &RgbImage, quality: u8) -> image::ImageResult<Vec<u8>> {
    let mut buf = Vec::with_capacity(128 * 1024);
    let encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality.clamp(1, 100));
    encoder.write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(buf)
}

fn detect_motion(state: &AppState, rgb: &RgbImage, prev: &mut Option<Vec<u8>>) -> Vec<MotionBox> {
    let (full_w, full_h) = rgb.dimensions();
    if full_w == 0 || full_h == 0 {
        return Vec::new();
    }
    let dw = state.config.motion_width.clamp(64, full_w.max(64));
    let dh = ((full_h as u64 * dw as u64 / full_w as u64).max(1)) as u32;

    let small = imageops::resize(rgb, dw, dh, imageops::FilterType::Triangle);
    let gray = imageops::grayscale(&small);
    let blurred = imageops::blur(&gray, 1.0);
    let current = blurred.into_raw();

    let previous = match prev.as_ref() {
        Some(p) if p.len() == current.len() => p.clone(),
        _ => {
            *prev = Some(current);
            return Vec::new();
        }
    };
    *prev = Some(current.clone());

    let threshold = state.config.motion_threshold;
    let mut binary: Vec<u8> = current
        .iter()
        .zip(previous.iter())
        .map(|(a, b)| u8::from(a.abs_diff(*b) > threshold))
        .collect();

    // Two 3x3 dilations roughly match OpenCV's dilate(..., iterations=2).
    dilate(&mut binary, dw as usize, dh as usize);
    dilate(&mut binary, dw as usize, dh as usize);

    let min_area = state.config.motion_min_area.max(1) as usize;
    let mut visited = vec![false; binary.len()];
    let mut result = Vec::new();
    let sw = dw as usize;
    let sh = dh as usize;

    for start in 0..binary.len() {
        if binary[start] == 0 || visited[start] {
            continue;
        }
        visited[start] = true;
        let mut stack = vec![start];
        let mut area = 0usize;
        let mut min_x = sw;
        let mut max_x = 0usize;
        let mut min_y = sh;
        let mut max_y = 0usize;

        while let Some(index) = stack.pop() {
            area += 1;
            let x = index % sw;
            let y = index / sw;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);

            let mut push = |nx: usize, ny: usize, stack: &mut Vec<usize>| {
                let next = ny * sw + nx;
                if !visited[next] && binary[next] == 1 {
                    visited[next] = true;
                    stack.push(next);
                }
            };
            if x > 0 {
                push(x - 1, y, &mut stack);
            }
            if x + 1 < sw {
                push(x + 1, y, &mut stack);
            }
            if y > 0 {
                push(x, y - 1, &mut stack);
            }
            if y + 1 < sh {
                push(x, y + 1, &mut stack);
            }
        }

        if area >= min_area {
            let sx = full_w as f64 / dw as f64;
            let sy = full_h as f64 / dh as f64;
            result.push(MotionBox {
                x: (min_x as f64 * sx) as u32,
                y: (min_y as f64 * sy) as u32,
                w: (((max_x - min_x + 1) as f64 * sx) as u32).max(1),
                h: (((max_y - min_y + 1) as f64 * sy) as u32).max(1),
            });
        }
    }

    result
}

fn dilate(binary: &mut [u8], width: usize, height: usize) {
    let source = binary.to_vec();
    for y in 0..height {
        for x in 0..width {
            let mut any = 0u8;
            'outer: for dy in -1i32..=1 {
                let ny = y as i32 + dy;
                if ny < 0 || ny >= height as i32 {
                    continue;
                }
                for dx in -1i32..=1 {
                    let nx = x as i32 + dx;
                    if nx < 0 || nx >= width as i32 {
                        continue;
                    }
                    if source[ny as usize * width + nx as usize] == 1 {
                        any = 1;
                        break 'outer;
                    }
                }
            }
            binary[y * width + x] = any;
        }
    }
}

fn draw_outline(image: &mut RgbImage, b: &MotionBox, thickness: u32, color: Rgb<u8>) {
    let (width, height) = image.dimensions();
    for t in 0..thickness {
        let y0 = b.y + t;
        let y1 = b.y + b.h.saturating_sub(1 + t);
        let x0 = b.x + t;
        let x1 = b.x + b.w.saturating_sub(1 + t);
        if y0 < height {
            for x in b.x..(b.x + b.w).min(width) {
                image.put_pixel(x, y0, color);
            }
        }
        if y1 < height {
            for x in b.x..(b.x + b.w).min(width) {
                image.put_pixel(x, y1, color);
            }
        }
        if x0 < width {
            for y in b.y..(b.y + b.h).min(height) {
                image.put_pixel(x0, y, color);
            }
        }
        if x1 < width {
            for y in b.y..(b.y + b.h).min(height) {
                image.put_pixel(x1, y, color);
            }
        }
    }
}

fn draw_watermark(image: &mut RgbImage, text: &str) {
    let height = image.height();
    let scale = (height / 360).clamp(1, 3);
    let text_w = text.chars().count() as u32 * 8 * scale;
    let text_h = 8 * scale;
    let x = 10i32;
    let y = height as i32 - text_h as i32 - 10;

    fill_rect(
        image,
        x - 4,
        y - 4,
        text_w as i32 + 8,
        text_h as i32 + 8,
        Rgb([0, 0, 0]),
    );

    let mut cursor = x;
    for ch in text.chars() {
        if let Some(rows) = BASIC_FONTS.get(ch) {
            for (row, bits) in rows.iter().enumerate() {
                for col in 0..8u32 {
                    if bits & (1u8 << col) != 0 {
                        fill_rect(
                            image,
                            cursor + (col * scale) as i32,
                            y + (row as u32 * scale) as i32,
                            scale as i32,
                            scale as i32,
                            Rgb([255, 255, 255]),
                        );
                    }
                }
            }
        }
        cursor += (8 * scale) as i32;
    }
}

fn fill_rect(image: &mut RgbImage, x: i32, y: i32, w: i32, h: i32, color: Rgb<u8>) {
    let (width, height) = image.dimensions();
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = (x + w).max(0) as u32;
    let y1 = (y + h).max(0) as u32;
    for py in y0..y1.min(height) {
        for px in x0..x1.min(width) {
            image.put_pixel(px, py, color);
        }
    }
}
