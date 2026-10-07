# Camera Web

一个基于 Flask + OpenCV 的摄像头监控 Web 服务。它把本地摄像头画面以 MJPEG 流的方式推送到浏览器，
并在网页上提供**实时预览、运动检测告警、定时抓拍录像、运动触发录像**等功能。

---

## 功能特性

| 功能 | 说明 |
| --- | --- |
| 实时预览 | 浏览器通过 `/video_feed` 接收 MJPEG 流，画面上叠加当前时间水印 |
| 运动检测 | 基于帧差法（灰度 + 高斯模糊 + 二值化 + 轮廓面积判断）检测画面变化 |
| 告警提示 | 检测到运动时，网页红框闪烁并播放蜂鸣声 |
| 定时抓拍 | 每隔 N 秒保存一张 JPEG 图片到 `recordings/` 目录 |
| 运动触发录像 | 仅在检测到运动时，按摄像头帧率连续保存 JPEG 图片 |
| 懒加载 | 摄像头在首次访问页面时才打开；无客户端且未录像时自动释放，节省资源 |
| 循环覆盖 | 图片使用 6 位递增编号，超过上限（172800 张）后从头覆盖，避免磁盘写满 |

---

## 环境要求

- **Python 3.7+**
- 一个可用的摄像头（默认使用系统摄像头索引 `0`）
- 操作系统：Windows / Linux / macOS 均可
  - Linux 下若需图形化调试，可能需要摄像头权限（如 `video` 用户组）

### 依赖

依赖列表见 `requirements.txt`：

```
Flask>=2.0
opencv-python-headless
numpy
```

> 说明：这里使用 `opencv-python-headless`，因为它不包含 GUI 组件，适合服务器/无桌面环境运行。
> 摄像头读取、编解码、保存图片所需功能均已包含。

---

## 安装与配置

### 1. 获取代码

```bash
git clone <仓库地址>
cd camera_web
```

### 2. 创建并激活虚拟环境（推荐）

Linux / macOS：

```bash
python3 -m venv cam_env
source cam_env/bin/activate        # 源码注释中使用的环境名即 cam_env
```

Windows（PowerShell）：

```powershell
python -m venv cam_env
.\cam_env\Scripts\Activate.ps1
```

### 3. 安装依赖

```bash
pip install -r requirements.txt
```

### 4. 配置项说明

当前配置均以代码常量形式写在 `camera_web.py` 中，如需修改请直接编辑对应位置并重启服务：

| 配置 | 位置 | 默认值 | 说明 |
| --- | --- | --- | --- |
| 摄像头索引 | `ensure_camera_started()` / `camera_loop()` 中的 `cv2.VideoCapture(0)` | `0` | 多摄像头时改成 `1`、`2` 等 |
| 录制间隔 | 全局变量 `recording_interval` | `10.0` 秒 | 也可在网页上通过输入框动态修改 |
| 图片保存上限 | `MAX_RECORDING_IMAGES` / `MAX_MOTION_RECORDING_IMAGES` | `172800` | 超过后编号循环覆盖 |
| 监听地址/端口 | `app.run(host='0.0.0.0', port=5000)`（文件最后一行） | `0.0.0.0:5000` | 修改方法见 [更改网页端口](#更改网页端口) |
| 录像根目录 | `os.path.join(os.getcwd(), 'recordings')` | `<工作目录>/recordings` | 启动时的工作目录下 |
| 运动检测灵敏度 | `camera_loop()` 中的阈值 `20`、轮廓面积下限 `50` | — | 越小越灵敏 |

---

## 运行

```bash
python camera_web.py
```

启动后终端会显示类似：

```
 * Running on http://0.0.0.0:5000
```

然后打开浏览器访问：

- 本机：<http://127.0.0.1:5000>
- 局域网其他设备：`http://<本机IP>:5000`

> 摄像头在**首次打开页面/视频流时**才会真正启动，因此启动服务本身不会占用摄像头。

### 声音提示

浏览器出于安全策略，需要一次用户交互才能播放声音。
首次打开页面后点击页面上的 **Enable Sound** 按钮即可启用运动告警蜂鸣；此后检测到运动时会自动响铃。

### 更改网页端口

端口写在 `camera_web.py` 的**最后一行**：

```python
if __name__ == '__main__':
    app.run(host='0.0.0.0', port=5000, debug=False)
```

#### 源码方式运行

把 `port=5000` 改成想要的端口（例如 `port=8080`），保存后重新启动即可：

```bash
python camera_web.py
```

之后访问 `http://<本机IP>:8080`。

#### 二进制方式运行

二进制里的端口是**编译时写死**的，无法通过命令行参数或配置文件修改。可选方案有三种：

1. **改源码后重新构建**：改好 `port` 并提交，再推一个新版本标签，CI 会重新构建并发布新的可执行文件
   （也可以本地按工作流中的 Nuitka 命令自行构建）。
2. **用反向代理转发（推荐，无需重新构建）**：让 Nginx 监听 80/443，把请求转发到 5000。

   ```nginx
   server {
       listen 80;
       server_name 192.168.1.100;

       location / {
           proxy_pass http://127.0.0.1:5000;
           proxy_http_version 1.1;
           proxy_buffering off;              # MJPEG 流必须关闭缓冲，否则画面会卡住
           proxy_set_header Host $host;
       }
   }
   ```

   之后访问 `http://192.168.1.100/`（80 端口）即可。
3. **临时端口映射**：用 SSH 隧道 `ssh -L 8080:127.0.0.1:5000 user@<服务器IP>`，
   然后在本机访问 `http://127.0.0.1:8080`。

> 改了端口后记得同步调整防火墙规则，例如 `sudo ufw allow 8080/tcp`。

---

## 使用 GitHub 构建的二进制程序

仓库配置了 GitHub Actions 工作流 `.github/workflows/nuitka-build.yml`，使用 **Nuitka** 把
`camera_web.py` 打包成**单个可执行文件**（onefile），目标机器无需安装 Python 与依赖。

### 触发构建

方式一，**推送版本标签**（会同时创建 Release 并上传产物）：

```bash
git tag v1.0.0
git push origin v1.0.0
```

方式二，**手动触发**：在仓库页面进入 `Actions` → `Nuitka Build (Ubuntu 22.04)` → `Run workflow`。
注意手动触发**不会**创建 Release，只能在本次运行的 `Artifacts` 中下载。

### 下载产物

- 标签触发：打开仓库的 `Releases` 页面，下载附件。文件名为 `camera_web-<标签>_ubuntu2204`，
  例如 `camera_web-v1.0.0_ubuntu2204`。
- 手动触发：在 `Actions` 该次运行的详情页底部 `Artifacts` 区域下载。

### 在目标机器上运行

产物是一个 **Linux x86_64 可执行文件**（在 Ubuntu 22.04 上构建，动态链接 glibc 2.35），
**不是 Windows 程序**，只能在 Linux 上运行。

```bash
# 1. 放到一个专门的目录（录像会保存到「启动时的工作目录」下的 recordings/）
mkdir -p ~/camera_web && cd ~/camera_web
mv ~/Downloads/camera_web-v1.0.0_ubuntu2204 .

# 2. 赋予执行权限
chmod +x camera_web-v1.0.0_ubuntu2204

# 3. 运行
./camera_web-v1.0.0_ubuntu2204
```

启动后与源码方式一致，访问 `http://<本机IP>:5000`。

### 后台运行

```bash
nohup ./camera_web-v1.0.0_ubuntu2204 > camera_web.log 2>&1 &
```

### 注册为 systemd 服务（开机自启）

新建 `/etc/systemd/system/camera-web.service`：

```ini
[Unit]
Description=Camera Web
After=network.target

[Service]
Type=simple
User=pi
WorkingDirectory=/home/pi/camera_web
ExecStart=/home/pi/camera_web/camera_web-v1.0.0_ubuntu2204
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now camera-web
sudo systemctl status camera-web     # 查看状态
journalctl -u camera-web -f          # 查看日志
```

### 取消开机自启

`enable` 与 `start` 是两件独立的事：**取消开机自启只影响下次开机是否自动运行，不会停止当前正在运行的服务**。

只取消开机自启（服务可继续运行，直到重启或手动停止）：

```bash
sudo systemctl disable camera-web
```

如果想**取消自启并立即停止**服务：

```bash
sudo systemctl disable --now camera-web
```

其他相关操作：

```bash
sudo systemctl stop camera-web          # 仅停止当前服务（不改自启设置）
sudo systemctl is-enabled camera-web    # 查看是否已设置开机自启（输出 enabled / disabled）
sudo systemctl is-active camera-web     # 查看当前是否在运行（输出 active / inactive）
```

完全移除该服务（连同 unit 文件）：

```bash
sudo systemctl disable --now camera-web
sudo rm /etc/systemd/system/camera-web.service
sudo systemctl daemon-reload
sudo systemctl reset-failed               # 清理残留的失败状态（可选）
```

> 说明：如果只是想临时手动跑，用 `sudo systemctl stop camera-web` 停止后，
> 直接在当前目录执行 `./camera_web-v1.0.0_ubuntu2204` 即可；
> 下次开机是否自动启动仍由 `enable` / `disable` 决定。

### 二进制使用注意事项

- **平台限制**：仅 Linux x86_64 / glibc ≥ 2.35（Ubuntu 22.04 及以上）可用。
  Debian 11、CentOS 7 等较老系统可能报 `GLIBC_2.35 not found`，请改用源码方式运行或自行重新构建。
- **端口固定为 5000**：端口在编译时写死，不可运行时修改，详见 [更改网页端口](#更改网页端口)。
- **摄像头**：程序固定使用索引 `0` 打开摄像头，没有命令行参数；
  运行用户需有摄像头访问权限（通常加入 `video` 组：`sudo usermod -aG video $USER`，重新登录后生效）。
- **录像目录**：为 `<启动时的工作目录>/recordings`，建议先 `cd` 到指定目录再启动，
  否则图片会散落在当前目录。
- **首次启动较慢**：Nuitka onefile 需要先把自身解压到临时目录，首次启动慢几秒属正常现象。
- **防火墙**：局域网访问需放行 5000 端口：

  ```bash
  sudo ufw allow 5000/tcp
  ```

---

## 页面操作

1. **实时画面**：页面顶部展示实时视频流。
2. **Motion Status**：显示 `Normal` 或 `Someone has entered.`，检测到运动时视频边框变红并响铃。
3. **定时录制（Recording）**
   - `Start Recording`：开始定时抓拍，并新建一个以时间命名的文件夹。
   - `录制间隔（秒）`：输入秒数后点击 `Set`（或按回车）即可动态调整抓拍间隔（最小 `0.01` 秒）。
   - `Stop Recording`：停止定时抓拍。
4. **运动触发录制（Motion Recording）**
   - `Start Motion Recording`：开始监听，仅在检测到运动时连续保存画面（按摄像头帧率）。
   - `Motion: YES (saving...)` 表示当前正在保存运动画面。
   - `Stop Motion Recording`：停止运动触发录制。

---

## 图片保存位置

所有录像保存在**启动服务时的当前工作目录**下的 `recordings/` 文件夹中：

```
recordings/
├── 20251007_143000/                 # 定时录制：以开始时间命名
│   ├── img-000000.jpg
│   ├── img-000001.jpg
│   └── ...
└── motion_20251007_150000/          # 运动触发录制：前缀 motion_
    ├── motion-000000.jpg
    ├── motion-000001.jpg
    └── ...
```

- 文件名中的 6 位数字为循环计数器，达到 `MAX_RECORDING_IMAGES` 后从头覆盖。
- 每张图片左下角都会写入对应的时间水印。

---

## HTTP 接口

服务提供以下接口，均可直接通过浏览器或 `curl` 调用：

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/` | 监控主页面 |
| GET | `/video_feed` | MJPEG 视频流 |
| GET | `/status` | 运动状态，返回 `{"motion": true/false}` |
| GET | `/recording_status` | 定时录制与运动录制状态、目录、当前间隔 |
| GET | `/start_recording` | 开始定时录制 |
| GET | `/stop_recording` | 停止定时录制 |
| GET | `/start_motion_recording` | 开始运动触发录制 |
| GET | `/stop_motion_recording` | 停止运动触发录制 |
| GET | `/motion_recording_status` | 运动录制状态与实时运动标志 |
| GET | `/set_recording_interval?interval=<秒>` | 设置定时录制间隔（需 ≥ 0.01） |

示例：

```bash
curl http://127.0.0.1:5000/status
curl "http://127.0.0.1:5000/set_recording_interval?interval=5"
```

---

## 常见问题

**1. 打开页面没有画面 / 一直黑屏**

- 检查摄像头是否被其他程序（如相机 App、其他监控软件）占用。
- 确认摄像头索引是否正确（默认 `0`，多摄像头设备需改索引）。
- 在 Linux 上确认当前用户对 `/dev/video*` 有访问权限。

**2. 页面提示无法访问 / 局域网访问不了**

- 确认防火墙放行了 5000 端口。
- 确认访问的是运行服务机器的局域网 IP，而非 `127.0.0.1`。

**3. 运动检测过于灵敏或不够灵敏**

- 修改 `camera_loop()` 中二值化阈值（默认 `20`）：调大更迟钝，调小更灵敏。
- 修改轮廓面积下限（默认 `50`）：调大可以忽略小范围抖动。

**4. 响铃没声音**

- 需先点击页面上的 **Enable Sound** 按钮（浏览器要求用户交互后才能播放音频）。

**5. 磁盘占用过大**

- 图片是逐张保存的，长时间运行会占用较多空间；可调大录制间隔、或调小
  `MAX_RECORDING_IMAGES` / `MAX_MOTION_RECORDING_IMAGES` 以更早循环覆盖。

---

## 注意事项

- 服务默认监听 `0.0.0.0`，同一网络内任何人都可访问，**请勿直接暴露到公网**。
- 生产环境部署建议在前面加一层反向代理（如 Nginx）并启用认证，或改用 `waitress`/`gunicorn` 等 WSGI 服务器。
