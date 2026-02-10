# -*- coding: utf-8 -*-
# source ~/cam_env/bin/activate

import cv2
import threading
import time
import os
from datetime import datetime
from flask import Flask, Response, render_template_string, jsonify, request

# ======================
# 全局变量
# ======================
# 延迟在首次访问时打开摄像头
camera = None
output_frame = None
lock = threading.Lock()

motion_detected = False

# 摄像头线程启动标志与锁，防止并发多次启动
camera_thread_started = False
camera_thread_lock = threading.Lock()

# 活跃客户端计数（连接到 /video_feed 的流媒体客户端数量）
active_clients = 0
active_clients_lock = threading.Lock()

# 录制相关
recording_active = False
recording_dir = None
recording_lock = threading.Lock()
recording_thread_started = False
recording_thread_lock = threading.Lock()
recording_event = threading.Event()
# 录制间隔（秒），默认10秒
recording_interval = 10.0

# ======================
# Flask Web
# ======================
app = Flask(__name__)

HTML_PAGE = """
<!doctype html>
<html>
<head>
    <meta charset="utf-8">
    <title>Camera Monitor</title>
    <style>
        #videoContainer { display:inline-block; border:6px solid transparent; }
        #videoContainer.alert { border-color: red; box-shadow: 0 0 20px red; }
    </style>
    </head>
<body>
    <h1>Live Camera</h1>
    <div id="videoContainer">
        <img id="video" src="/video_feed" width="1080">
    </div>

    <h2>Motion Status:</h2>
    <p id="status">Normal</p>
    <h2>Recording: <span id="rec_interval_display"></span></h2>
    <p id="rec_status">Stopped</p>
    <p id="rec_dir"></p>
    <p>
        录制间隔（秒）：<input id="rec_interval" type="number" min="0.01" max="3600" step="0.01" style="width:6rem;"> 
        <button id="setInterval">Set</button>
    </p>
    <button id="startRec" style="display:inline-block;margin-right:1rem;">Start Recording</button>
    <button id="stopRec" style="display:inline-block;">Stop Recording</button>
    <button id="enableSound" style="display:inline-block;margin-left:1rem;">Enable Sound</button>

    <script>
        // Web Audio beep (persistent AudioContext activated on user gesture)
        let audioCtx = null;
        function initAudio() {
            if (!audioCtx) {
                try {
                    audioCtx = new (window.AudioContext || window.webkitAudioContext)();
                } catch (e) {
                    audioCtx = null;
                }
            }
            if (audioCtx && audioCtx.state === 'suspended') {
                audioCtx.resume().catch(()=>{});
            }
        }

        // Provide a visible control to enable sound (helps user gesture requirement)
        const enableBtn = document.getElementById('enableSound');
        function enableSoundHandler() {
            initAudio();
            enableBtn.style.display = 'none';
        }
        enableBtn.addEventListener('click', enableSoundHandler, {once:true});
        enableBtn.addEventListener('touchstart', enableSoundHandler, {once:true});

        function playBeep() {
            try {
                if (!audioCtx) initAudio();
                if (!audioCtx) return;
                const now = audioCtx.currentTime;
                const o = audioCtx.createOscillator();
                const g = audioCtx.createGain();
                o.type = 'sine';
                o.frequency.setValueAtTime(880, now);
                g.gain.setValueAtTime(0.0, now);
                // increase peak gain for louder beep
                g.gain.linearRampToValueAtTime(1.8, now + 0.01);
                g.gain.exponentialRampToValueAtTime(0.0001, now + 0.4);
                o.connect(g);
                g.connect(audioCtx.destination);
                o.start(now);
                o.stop(now + 0.45);
                o.onended = () => {
                    try { o.disconnect(); } catch(e){}
                    try { g.disconnect(); } catch(e){}
                };
            } catch (e) {
                // ignore if WebAudio not available or blocked
            }
        }

        let alerting = false;
        setInterval(function () {
            fetch('/status')
                .then(response => response.json())
                .then(data => {
                    const text = data.motion ? "Someone has entered." : "Normal";
                    document.getElementById('status').innerText = text;
                    const container = document.getElementById('videoContainer');
                    if (data.motion) {
                        if (!alerting) {
                            alerting = true;
                            container.classList.add('alert');
                            playBeep();
                            // keep alert for 5s
                            setTimeout(() => {
                                container.classList.remove('alert');
                                alerting = false;
                            }, 5000);
                        }
                            // keep alert for 5s
                            setTimeout(() => {
                                container.classList.remove('alert');
                                alerting = false;
                            }, 5000);
                    } else {
                        container.classList.remove('alert');
                        alerting = false;
                    }
                }).catch(()=>{});
        }, 800);

        // Recording controls: register once and poll status periodically
        (function(){
            const startBtn = document.getElementById('startRec');
            const stopBtn = document.getElementById('stopRec');
            const recIntervalInput = document.getElementById('rec_interval');
            let recEditing = false;

            recIntervalInput.addEventListener('focus', () => { recEditing = true; });
            recIntervalInput.addEventListener('blur', () => { recEditing = false; });

            function refreshRecStatus() {
                fetch('/recording_status')
                    .then(r => r.json()).then(data => {
                        document.getElementById('rec_status').innerText = data.recording ? 'Recording' : 'Stopped';
                        document.getElementById('rec_dir').innerText = data.dir ? ('Folder: ' + data.dir) : '';
                        // update interval input & display if provided and user is not editing
                        if (data.interval !== undefined) {
                            if (!recEditing) {
                                recIntervalInput.value = data.interval;
                            }
                            // always update visible display so user sees current value
                            try {
                                const v = parseFloat(data.interval);
                                document.getElementById('rec_interval_display').innerText = isFinite(v) ? (v + ' s') : '';
                            } catch (e) { }
                        }
                        startBtn.style.display = data.recording ? 'none' : 'inline-block';
                        stopBtn.style.display = data.recording ? 'inline-block' : 'none';
                    }).catch(()=>{});
            }

            startBtn.addEventListener('click', function () {
                fetch('/start_recording').then(r=>r.json()).then(()=>refreshRecStatus()).catch(()=>{});
            });
            stopBtn.addEventListener('click', function () {
                fetch('/stop_recording').then(r=>r.json()).then(()=>refreshRecStatus()).catch(()=>{});
            });

            // set interval control
            const setBtn = document.getElementById('setInterval');
            function submitInterval() {
                const val = parseFloat(recIntervalInput.value);
                if (!isFinite(val) || val < 0.01) return;
                fetch('/set_recording_interval?interval=' + encodeURIComponent(val)).then(r=>r.json()).then(()=>refreshRecStatus()).catch(()=>{});
                recIntervalInput.blur();
            }
            setBtn.addEventListener('click', submitInterval);
            recIntervalInput.addEventListener('keydown', function(e) {
                if (e.key === 'Enter') {
                    submitInterval();
                }
            });

            // initial status and periodic refresh
            refreshRecStatus();
            setInterval(refreshRecStatus, 1000);
        })();
    </script>
</body>
</html>
"""


@app.route('/')
def index():
    return render_template_string(
        HTML_PAGE,
        status="Someone has entered." if motion_detected else "normal"
    )

@app.route('/status')
def status():
    return jsonify({'motion': bool(motion_detected)})


@app.route('/recording_status')
def recording_status():
    return jsonify({'recording': bool(recording_active), 'dir': recording_dir or '', 'interval': recording_interval})


@app.route('/start_recording')
def start_recording():
    global recording_active, recording_dir
    ensure_camera_started()

    # 等待首帧可用
    waited = 0
    while True:
        with lock:
            frame = None if output_frame is None else output_frame.copy()
        if frame is not None:
            break
        time.sleep(0.05)
        waited += 0.05
        if waited > 5:
            break

    # 创建新的文件夹（以当前时间为初始帧时间）
    now = datetime.now()
    folder_name = now.strftime('%Y%m%d_%H%M%S')
    base_dir = os.path.join(os.getcwd(), 'recordings')
    os.makedirs(base_dir, exist_ok=True)
    dir_path = os.path.join(base_dir, folder_name)
    os.makedirs(dir_path, exist_ok=True)

    with recording_lock:
        recording_dir = dir_path
        recording_active = True
        recording_event.set()

    ensure_recording_started()
    return jsonify({'started': True, 'dir': recording_dir})


@app.route('/stop_recording')
def stop_recording():
    global recording_active
    with recording_lock:
        recording_active = False
        recording_event.clear()
    return jsonify({'stopped': True})


@app.route('/set_recording_interval')
def set_recording_interval():
    global recording_interval
    val = request.args.get('interval', None)
    if val is None:
        return jsonify({'success': False, 'error': 'missing interval parameter'}), 400
    try:
        f = float(val)
    except Exception:
        return jsonify({'success': False, 'error': 'invalid interval'}), 400
    if f < 0.01:
        return jsonify({'success': False, 'error': 'interval must be >= 0.01'}), 400
    with recording_lock:
        recording_interval = f
    return jsonify({'success': True, 'interval': recording_interval})

def generate():
    global output_frame, active_clients, camera

    # 注册为活跃客户端
    with active_clients_lock:
        active_clients += 1

    # 确保摄像头已启动（如果需要的话会启动线程并打开摄像头）
    ensure_camera_started()

    try:
        while True:
            with lock:
                frame = None if output_frame is None else output_frame.copy()

            if frame is None:
                time.sleep(0.01)
                continue

            ret, jpeg = cv2.imencode('.jpg', frame)
            if not ret:
                time.sleep(0.01)
                continue

            yield (b'--frame\r\n'
                   b'Content-Type: image/jpeg\r\n\r\n' +
                   jpeg.tobytes() + b'\r\n')
    except GeneratorExit:
        # 客户端断开时会抛出 GeneratorExit，继续到 finally
        pass
    finally:
        # 注销活跃客户端；如果没有剩余客户端则关闭摄像头释放资源
        with active_clients_lock:
            active_clients -= 1
            remaining = active_clients

        if remaining == 0:
            try:
                if camera is not None:
                    camera.release()
            except Exception:
                pass
            camera = None

@app.route('/video_feed')
def video_feed():
    return Response(generate(),
                    mimetype='multipart/x-mixed-replace; boundary=frame')

# 确保摄像头已打开并启动摄像头线程（若尚未启动）
def ensure_camera_started():
    global camera, camera_thread_started
    with camera_thread_lock:
        if camera is None:
            try:
                camera = cv2.VideoCapture(0)
            except Exception:
                camera = None
        if not camera_thread_started:
            t = threading.Thread(target=camera_loop, daemon=True)
            t.start()
            camera_thread_started = True

# ======================
# 摄像头 + 运动检测
# ======================
def camera_loop():
    global output_frame, motion_detected, camera

    prev_frame = None

    while True:
        # 如果摄像头尚未初始化或为 None，短暂休眠等待
        if camera is None:
            time.sleep(0.1)
            continue

        # 如果摄像头未打开，尝试释放并重建 VideoCapture
        try:
            opened = camera.isOpened()
        except Exception:
            opened = False

        if not opened:
            try:
                camera.release()
            except Exception:
                pass
            try:
                camera = cv2.VideoCapture(0)
            except Exception:
                camera = None
            time.sleep(1)
            continue

        ret, frame = camera.read()
        if not ret:
            # 读帧失败：释放并重建摄像头，然后短暂等待
            try:
                camera.release()
            except:
                pass
            camera = cv2.VideoCapture(0)
            time.sleep(1)
            continue

        gray = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)
        gray = cv2.GaussianBlur(gray, (21, 21), 0)

        if prev_frame is None:
            prev_frame = gray
            # 在首次帧上添加时间水印
            try:
                timestamp = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime())
                h, w = frame.shape[:2]
                font = cv2.FONT_HERSHEY_SIMPLEX
                scale = 0.6
                thickness = 2
                text_size = cv2.getTextSize(timestamp, font, scale, thickness)[0]
                x = 10
                y = h - 10
                # 黑色底色以提高可读性
                cv2.putText(frame, timestamp, (x, y), font, scale, (0, 0, 0), thickness + 2, cv2.LINE_AA)
                cv2.putText(frame, timestamp, (x, y), font, scale, (255, 255, 255), thickness, cv2.LINE_AA)
            except Exception:
                pass

            with lock:
                output_frame = frame.copy()
            time.sleep(0.03)
            continue

        frame_delta = cv2.absdiff(prev_frame, gray)
        thresh = cv2.threshold(frame_delta, 20, 255, cv2.THRESH_BINARY)[1]
        thresh = cv2.dilate(thresh, None, iterations=2)

        contours, _ = cv2.findContours(
            thresh, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)

        motion_detected = False
        for c in contours:
            if cv2.contourArea(c) < 50:
                continue
            motion_detected = True
            (x, y, w, h) = cv2.boundingRect(c)
            cv2.rectangle(frame, (x, y), (x + w, y + h), (0, 0, 255), 2)

        # 前端负责播放声音和闪红边框，服务器端不再发声。

        prev_frame = gray.copy()

        # 在输出帧左下角添加时间水印
        try:
            timestamp = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime())
            fh, fw = frame.shape[:2]
            font = cv2.FONT_HERSHEY_SIMPLEX
            scale = 0.6
            thickness = 2
            x = 10
            y = fh - 10
            cv2.putText(frame, timestamp, (x, y), font, scale, (0, 0, 0), thickness + 2, cv2.LINE_AA)
            cv2.putText(frame, timestamp, (x, y), font, scale, (255, 255, 255), thickness, cv2.LINE_AA)
        except Exception:
            pass

        with lock:
            output_frame = frame.copy()

        time.sleep(0.03)


# ======================
# 录制后台线程（每隔10秒保存一张图片）
# ======================
def recording_loop():
    global recording_active, recording_dir

    last_saved = 0

    while True:
        # 等待一会儿或直到录制被触发（短超时以支持高频率保存）
        recording_event.wait(timeout=0.01)

        if not recording_active:
            time.sleep(0.2)
            continue

        # 取一帧并保存
        with lock:
            frame = None if output_frame is None else output_frame.copy()

        if frame is None:
            # 当帧不可用时短睡，避免长时间阻塞
            time.sleep(0.01)
            continue

        now_ts = time.time()
        # 首次保存或者到达设置的间隔
        with recording_lock:
            interval = recording_interval
        if last_saved == 0 or (now_ts - last_saved) >= interval:
            try:
                tstr = time.strftime('%Y%m%d_%H%M%S', time.localtime(now_ts))
                filename = f"img-{tstr}.jpg"
                with recording_lock:
                    target_dir = recording_dir
                if target_dir:
                    path = os.path.join(target_dir, filename)
                    cv2.imwrite(path, frame)
                    last_saved = now_ts
            except Exception:
                pass

        # 根据设置的间隔调整短睡，避免忙循环同时支持高频率
        try:
            sleep_time = max(0.001, min(0.05, interval / 2.0))
        except Exception:
            sleep_time = 0.01
        time.sleep(sleep_time)


def ensure_recording_started():
    global recording_thread_started
    with recording_thread_lock:
        if not recording_thread_started:
            t = threading.Thread(target=recording_loop, daemon=True)
            t.start()
            recording_thread_started = True

# ======================
# 主程序
# ======================
if __name__ == '__main__':
    app.run(host='0.0.0.0', port=5000, debug=False)
