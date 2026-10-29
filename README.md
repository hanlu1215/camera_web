# Camera Web (Rust)

一个用 **Rust + Axum + ffmpeg** 实现的摄像头监控 Web 服务。浏览器通过 MJPEG 实时查看画面，
服务端负责运动检测、定时抓拍、运动触发录像；网页上还可以**一键把图片序列转成 MP4**、
**每天定时自动把图片转成视频并开启新的录像**，以及**在线观看历史录像**。

> `camera_web.py` 是旧的 Python 实现，仅作参考保留；构建、部署和运行均以 Rust 版本为准。

---

## 功能特性

| 功能 | 说明 |
| --- | --- |
| 实时预览 | `/video_feed` 输出 MJPEG，画面带时间水印与运动框，多客户端共享同一份编码帧 |
| 运动检测 | 降采样灰度帧差 + 阈值 + 膨胀 + 连通域面积判断（阈值/面积/检测宽度可配置） |
| 告警提示 | 网页红框闪烁并播放蜂鸣声（需先点一次 Enable Sound） |
| 定时抓拍 | 每隔 N 秒保存一张带水印的 JPEG，间隔可在网页上动态修改 |
| 运动触发录像 | 仅在检测到运动时，按摄像头帧率连续保存 JPEG |
| 一键转 MP4 | 图片序列编码为 MP4 并保存到 `recordings/Video/`；校验成功后**删除整个原录制文件夹** |
| 自动转换 | 每天指定时间（默认 03:00）自动轮换当前录像、转换所有已结束的图片文件夹 |
| 历史录像 | 网页列出所有录像/视频，支持在线播放和拖动进度（HTTP Range） |
| 删除 | 网页上可删除录像文件夹或单个视频（录制中/转换中的会拒绝删除） |
| 懒加载 | 没有观看者且没有录像任务时自动关闭摄像头，节省资源 |
| 循环覆盖 | 图片编号 6 位循环，单文件夹最多 172800 张，超过后从头覆盖 |

---

## 依赖与运行环境

### 运行时（必须）

- Linux（使用 `v4l2` 采集，设备路径为 `/dev/videoN`）
- **ffmpeg**：同时用于摄像头采集和图片转 MP4；`ffprobe` 用于转换后的文件校验（可选但强烈建议）
- 对 `/dev/videoN` 的读权限（systemd 服务以 root 运行时默认满足；普通用户需加入 `video` 组）
- 运行目录需可写（保存录像和设置文件）

```bash
# Debian / Ubuntu
sudo apt-get install -y ffmpeg
```

### 构建（源码方式）

- Rust stable（建议 1.85+）；不需要 OpenCV、libclang、X11 等系统开发库
- 首次构建会下载 crates.io 依赖

---

## 构建与运行

```bash
git clone <仓库地址>
cd camera_web

cargo build --release
./target/release/camera_web
```

默认行为：

- 监听 `0.0.0.0:5000`
- 录像目录：`./recordings`（相对**启动时的工作目录**）
- 设置文件：`./camera_web_settings.json`
- 摄像头：`/dev/video0`，请求 1280x720 @ 30fps

打开浏览器访问 `http://<本机IP>:5000`。

### 常用示例

```bash
# 指定摄像头、分辨率、端口、录像目录
./target/release/camera_web \
  --camera 1 --width 1920 --height 1080 --fps 25 \
  --port 8080 --recordings-dir /data/camera/recordings

# 没有摄像头时用 ffmpeg 测试图案联调网页/录像/转换
./target/release/camera_web --test-pattern --port 5010
```

### 配置文件（换目录只改这一处）

服务启动时会读取 `--config` 指定的配置文件（默认 `./camera_web_config.json`，不存在时自动生成示例）。
录像根目录、导出视频目录、运行时设置文件都集中在这里：

```json
{
  "_comment": "改动路径后重启 camera-web 即可。相对路径相对于启动时的工作目录。",
  "recordings_dir": "/vol1/1000/TEMP/recordings",
  "video_dir": "/vol1/1000/TEMP/recordings/Video",
  "settings": "/vol1/1000/TEMP/camera_web_settings.json"
}
```

| 字段 | 说明 |
| --- | --- |
| `recordings_dir` | 录制文件夹（`img-*.jpg` / `motion-*.jpg`）的根目录 |
| `video_dir` | 转换后 MP4 的导出目录；可以放在 `recordings_dir` 之外（服务会自动在历史页列出并通过 `/media-video` 提供播放） |
| `settings` | 运行时设置文件（自动转换/间隔） |

> 修改配置文件后需要重启服务。命令行 `--recordings-dir` / `--video-dir` / `--settings` 仍可作为临时覆盖，优先级高于配置文件。

### 命令行配置项

| 参数 | 默认值 | 说明 |
| --- | --- | --- |
| `--config <FILE>` | `./camera_web_config.json` | 启动配置文件（路径集中在这里） |
| `--camera <N>` | `0` | 摄像头索引，对应 `/dev/videoN` |
| `--width <W>` | `1280` | 采集/输出宽度（ffmpeg 会缩放） |
| `--height <H>` | `720` | 采集/输出高度 |
| `--fps <F>` | `30` | 请求帧率，同时作为输出节流上限 |
| `--bind <ADDR>` | `0.0.0.0` | HTTP 监听地址 |
| `--port <PORT>` | `5000` | HTTP 端口 |
| `--recordings-dir <DIR>` | 配置文件 / `./recordings` | 录制文件夹根目录（覆盖配置文件） |
| `--video-dir <DIR>` | 配置文件 / `<recordings>/Video` | 导出视频目录（覆盖配置文件） |
| `--settings <FILE>` | 配置文件 / `./camera_web_settings.json` | 持久化设置文件（覆盖配置文件） |
| `--ffmpeg <PATH>` | `ffmpeg` | ffmpeg 可执行文件路径 |
| `--ffmpeg-threads <N>` | `2` | 转换线程数，调高会更快但可能影响实时采集 |
| `--jpeg-quality <1-100>` | `80` | 实时/录像 JPEG 质量 |
| `--motion-threshold <0-255>` | `20` | 帧差阈值，越小越灵敏 |
| `--motion-min-area <PX>` | `50` | 连通域最小面积（在降采样图上），越大越不敏感 |
| `--motion-width <PX>` | `320` | 运动检测使用的降采样宽度 |
| `--camera-retry-secs <S>` | `3.0` | 摄像头/采集进程失败后的重试间隔 |
| `--start-recording` | 关闭 | 启动服务时立即开始定时抓拍 |
| `--start-motion-recording` | 关闭 | 启动服务时立即开启运动触发录像（推荐配合 systemd 使用，重启后继续录像） |
| `--test-pattern` | 关闭 | 隐藏参数：用 ffmpeg 合成测试画面代替摄像头 |

设置文件示例（首次启动自动生成；网页上的自动转换开关/时间/FPS 会写回这里）：

```json
{
  "auto_convert_enabled": false,
  "auto_convert_time": "03:00",
  "convert_fps": 30.0,
  "recording_interval": 10.0
}
```

> 自动转换默认**关闭**。启用后会在每天的本地 `auto_convert_time` 执行；如果服务在该时间点没有运行，
> 会在下一个设定时间执行。

---

## 网页功能

1. **实时画面**：顶部 MJPEG 流；检测到运动时边框变红、播放蜂鸣。
2. **定时抓拍**：设置间隔（最小 0.01 秒），Start/Stop。
3. **运动触发录像**：Start Motion Recording 后，只在有运动时保存。
4. **历史录像**：
   - 按时间倒序列出录像和视频；图片文件夹显示缩略图与图片数量。
   - H.264 等浏览器支持的视频可直接点「播放」，支持拖动进度。
   - 图片文件夹可点「转为 MP4」：转换在后台队列执行，完成后 MP4 出现在 `recordings/Video/`，
     **原录制文件夹（含图片）会被删除**。
   - 「转换全部待处理」批量处理图片文件夹。
   - 录像文件夹/视频旁有「删除」按钮（需二次确认；录制中或转换中的对象不可删除）。
5. **自动转换设置**：勾选「每天自动转换」，设置时间与视频 FPS，保存后写入设置文件。
   到点后服务会：
   1. 若定时/运动录像正在运行，立即关闭当前文件夹并**在同目录下开启新文件夹继续录制**；
   2. 把所有已结束且包含图片的文件夹加入转换队列；
   3. 逐个转换，校验通过后在 `video_dir`（默认 `recordings/Video/`）生成 `<文件夹名>.mp4`，并删除原录制文件夹。

### 转换 FPS 怎么选

- **运动触发录像**：图片是按摄像头帧率连续保存的，用 `30`（或与采集帧率一致）即可得到接近实时的视频。
- **定时抓拍**：例如每 10 秒一张，用 `30fps` 会得到 300 倍速的延时视频；想让时长贴近真实时间，
  可以把 FPS 设为 `1 / 间隔`（如 `0.1`），但极低帧率的视频在部分播放器上体验较差。
- 批量/自动转换统一使用设置里的 `convert_fps`；单个文件夹也可以调用 API 时指定 `fps`。

---

## 文件布局

```
recordings/
├── 20261007_143000/                      # 录制中/待转换：以开始时间命名
│   ├── img-000000.jpg
│   ├── img-000001.jpg
│   └── ...
├── motion_20261007_150000/               # 运动触发：motion_ 前缀
│   └── motion-000000.jpg
└── Video/                                # 默认导出目录（可在 camera_web_config.json 改成其他路径）
    ├── 20261007_143000.mp4               # 转换成功后原录制文件夹已删除
    ├── motion_20261007_150000.mp4
    └── old_recording.mp4                 # 历史遗留视频
```

- 图片左下角带时间水印；运动框为红色矩形。
- 6 位计数器到达上限（172800）后从 0 覆盖，旧图会被新图替换。
- 转换先写 `recordings/Video/` 下的隐藏临时文件 `.<文件夹名>.part`，校验通过后重命名为 `<文件夹名>.mp4`，
  然后递归删除原录制文件夹。转换失败会保留原文件夹并清理临时文件。

---

## HTTP 接口

### 新接口（JSON）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/` | 监控页面 |
| GET | `/video_feed` | MJPEG 视频流 |
| GET | `/api/status` | 运动状态、摄像头是否活跃、客户端数、最新帧年龄 |
| GET | `/api/recording/status` | 定时/运动录像状态、目录、间隔、自动转换配置 |
| POST | `/api/recording/start` | 开始定时抓拍 |
| POST | `/api/recording/stop` | 停止定时抓拍 |
| POST | `/api/recording/interval` | 设置抓拍间隔，JSON `{"interval": 5}` |
| POST | `/api/motion_recording/start` | 开始运动触发录像 |
| POST | `/api/motion_recording/stop` | 停止运动触发录像 |
| GET | `/api/motion_recording_status` | 运动录像状态与实时运动标志 |
| GET | `/api/history` | 历史文件夹/视频列表（含缩略图、大小、转换状态） |
| POST | `/api/convert` | 转换单个文件夹，JSON `{"path":"20261007_143000","fps":30}` |
| POST | `/api/convert/all` | 转换所有已结束的图片文件夹 |
| POST | `/api/delete` | 删除录像文件夹或 MP4，JSON `{"path":"20261007_143000"}`；导出目录中的视频用 `@video/<文件名>` |
| GET | `/api/conversions` | 最近的转换任务状态 |
| GET | `/api/settings` | 读取自动转换/间隔设置与下次执行时间 |
| POST | `/api/settings` | 修改设置（字段均可选） |
| GET | `/api/health` | 健康检查（版本、运行时长、录像/视频目录、ffmpeg 可用性） |
| GET | `/media/<相对路径>` | 访问录制目录中的文件，支持 Range 请求 |
| GET | `/media-video/<相对路径>` | 访问 `video_dir` 中的导出视频，支持 Range 请求 |

### 兼容旧版接口

旧 Python 版的接口全部保留，便于已有脚本和书签继续使用：

| 方法 | 路径 |
| --- | --- |
| GET | `/status` |
| GET | `/recording_status` |
| GET | `/start_recording` |
| GET | `/stop_recording` |
| GET | `/start_motion_recording` |
| GET | `/stop_motion_recording` |
| GET | `/motion_recording_status` |
| GET | `/set_recording_interval?interval=<秒>` |

示例：

```bash
curl http://127.0.0.1:5000/api/health
curl -X POST http://127.0.0.1:5000/api/recording/start
curl -X POST -H 'Content-Type: application/json' \
     -d '{"path":"20261007_143000","fps":30}' \
     http://127.0.0.1:5000/api/convert
```

---

## 二进制部署

仓库配置了 GitHub Actions 工作流 `.github/workflows/rust-build.yml`：

- 推送 `v*.*.*` 标签：构建并创建 Release，产物名为 `camera_web-<标签>_ubuntu2204`
- 手动触发 `workflow_dispatch`：只上传 Actions Artifact，不创建 Release

产物是在 Ubuntu 22.04 上构建的 Linux x86_64 可执行文件（动态链接 glibc 2.35+），
因此目标系统需要 glibc ≥ 2.35 且已安装 **ffmpeg**（安装包不包含 ffmpeg）。

```bash
# 目标机器
mkdir -p ~/camera_web && cd ~/camera_web
mv camera_web-v0.2.0_ubuntu2204 camera_web_bin
chmod +x camera_web_bin
sudo apt-get install -y ffmpeg
./camera_web_bin
```

### systemd 服务

仓库内提供了与当前部署一致的服务文件：`deploy/camera-web.service`。

```ini
[Unit]
Description=Camera Web Service (Rust)
After=network.target

[Service]
Type=simple
ExecStart=/vol1/1000/TEMP/camera_web_bin --config /vol1/1000/TEMP/camera_web_config.json --port 5000 --start-motion-recording
WorkingDirectory=/vol1/1000/TEMP
Restart=always
RestartSec=5
User=root
Environment=RUST_LOG=info
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

安装/更新服务：

```bash
cd /vol1/1000/TEMP/CODE_SRC/camera_web
cargo build --release
sudo ./deploy/install-service.sh
```

或手动执行：

```bash
sudo install -m 0755 target/release/camera_web /vol1/1000/TEMP/camera_web_bin
sudo install -m 0644 deploy/camera-web.service /etc/systemd/system/camera-web.service
sudo systemctl daemon-reload
sudo systemctl restart camera-web
sudo systemctl status camera-web
journalctl -u camera-web -f          # 查看日志
```

> 也可以只替换二进制（无需改 unit 文件）：先 `sudo systemctl stop camera-web`，
> 覆盖 `/vol1/1000/TEMP/camera_web_bin`，再 `sudo systemctl start camera-web`。

---

## 从旧 Python 版本迁移

1. 停止旧服务：`sudo systemctl stop camera-web`。
2. 用 Rust 二进制替换原来的 `camera_web_bin`（建议先备份）。
3. 确认目标机器安装 ffmpeg：`ffmpeg -version`。
4. 录像目录沿用原来的 `recordings/`，无需迁移；历史文件夹和已有 MP4 都会显示在新网页中。
5. 启动服务并访问网页，按需开启「每天自动转换」。

> 旧的 Nuitka 构建工作流已移除，改为 `rust-build.yml`。
> 新版本不会从旧进程继承「运动录像已开启」的状态；部署用的 unit 文件带有
> `--start-motion-recording`，服务启动后会立即新建一个运动录像文件夹继续录制。

---

## 常见问题

**1. 网页没有实时画面**

- 运行 `ffmpeg -f v4l2 -i /dev/video0 -frames:v 1 /tmp/test.jpg` 检查摄像头与权限。
- 确认 ffmpeg 在服务的 `PATH` 中（systemd 下默认包含 `/usr/bin`）。
- 用 `--test-pattern` 启动，验证网页/录像/转换链路是否正常。
- 查看日志：`journalctl -u camera-web -n 100 --no-pager`。

**2. 转换失败 / 一直排队**

- 查看 `GET /api/conversions` 中的 `message`，或服务日志。
- 确认 `ffmpeg`、`ffprobe` 可执行；磁盘有足够空间。
- 转换是串行队列，单个大文件夹（十几万张图）或长视频可能耗时较久。

**2.1 点击播放没有反应 / 黑屏**

- 新版转换出的视频是 H.264，可直接播放。
- 如果手动放入的视频使用了浏览器不支持的编码（如 MPEG-4 Part 2、HEVC），需要先用 ffmpeg 离线转换。

**3. 自动转换没有执行**

- 网页上的开关是否已勾选并保存。
- 时间是**服务器本地时间**；在设定时间之后才启用，下一次是明天。
- 修改设置会重新计算下一次执行时间。

**4. 磁盘占用过大**

- 使用「转换全部待处理」或自动转换，把图片压缩成 MP4 并删除原图。
- 增大抓拍间隔，或在 `src/recording.rs` 中调小 `MAX_RECORDING_IMAGES` / `MAX_MOTION_RECORDING_IMAGES`。

**5. 转换速度慢、影响实时画面**

- 降低 `--ffmpeg-threads`（默认 2），或把转换安排在凌晨的自动任务里。
- 关闭不必要的浏览器页面，减少 MJPEG 客户端数量。

---

## 注意事项

- 服务默认监听 `0.0.0.0`，**没有任何认证**，请勿直接暴露到公网；建议放在带认证的 Nginx 后面。
- 图片转 MP4 会**删除整个原录制文件夹**（仅在 ffmpeg 成功且校验通过后）；重要录像请先备份。
- 删除操作不可恢复，网页上会二次确认。
- 图片按 6 位计数器循环覆盖；长期运行请结合自动转换控制磁盘占用。
- 摄像头由 ffmpeg 子进程打开；服务重启或释放摄像头时，ffmpeg 进程会被一并结束。
- 运动检测参数（阈值、面积、检测宽度）以命令行参数为准，调整后需要重启服务。
