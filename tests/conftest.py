# -*- coding: utf-8 -*-
"""SerialHub Sprint1 QA 一致性套件 — 公共夹具与工具。

黑盒纪律 (goal-qa):
- 预期只来自 docs/product/spec.md 的 FR/PLAT/PERF 条目 + FR-4 末尾 ADR-5 契约裁定;
  不读 src/ 实现"调预期", 失败如实记录, 不改预期凑绿。
- 被测桥 = target/release/serialhub.exe, 由夹具全生命周期管理:
  每条测试拉起 → 测试体运行 → teardown 强杀, 并有会话级兜底, 绝不残留占 COM1 的进程。
- 串口只允许 COM1(桥侧) ↔ COM2(pyserial 对端) ELTIMA 虚拟对; 全程禁止碰 COM8。

运行: 在项目根执行  python -m pytest tests/ -v
"""
from __future__ import annotations

import asyncio
import itertools
import json
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

import pytest
import serial
import websockets

PROJECT_ROOT = Path(__file__).resolve().parents[1]
BRIDGE_EXE = PROJECT_ROOT / "target" / "release" / "serialhub.exe"
BRIDGE_COM = "COM1"  # 桥侧 (硬约束: 测试只许用 COM1/COM2)
PEER_COM = "COM2"    # pyserial 对端
HTTP_HOST = "127.0.0.1"

# ADR-5 ①: /api/status 恰好 9 字段, 不含 flow
STATUS_FIELDS = {"phase", "port", "baud", "config", "clients",
                 "rxBytes", "txBytes", "lastError", "uptimeSec"}


# ------------------------------------------------------------------ 基础工具

def kill_all_bridges() -> None:
    """兜底清理: 强杀本机所有 serialhub.exe (仅本项目被测二进制, 防残留占 COM1)。"""
    subprocess.run(["taskkill", "/F", "/IM", "serialhub.exe", "/T"],
                   capture_output=True, check=False)


def free_tcp_port() -> int:
    s = socket.socket()
    s.bind((HTTP_HOST, 0))
    port = s.getsockname()[1]
    s.close()
    return port


def http_get(url: str, timeout: float = 5.0):
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return r.status, json.loads(r.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, raw


def http_post(url: str, body, timeout: float = 5.0):
    data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(url, data=data,
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.loads(r.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, raw


def poll_until(fn, timeout: float = 5.0, interval: float = 0.1, desc: str = "条件"):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = fn()
        if last:
            return last
        time.sleep(interval)
    raise AssertionError(f"{timeout}s 内未满足: {desc} (最后值={last!r})")


def _tail(path: Path, n: int = 2000) -> str:
    try:
        return path.read_text("utf-8", "replace")[-n:]
    except OSError:
        return "<无日志>"


# ------------------------------------------------------------------ 被测桥管理

class Bridge:
    """一个被测桥进程 + 其 HTTP/WS 入口。"""

    def __init__(self, proc: subprocess.Popen, http_port: int, log_path: Path):
        self.proc = proc
        self.http_port = http_port
        self.base = f"http://{HTTP_HOST}:{http_port}"
        self.ws_url = f"ws://{HTTP_HOST}:{http_port}/ws"
        self.log_path = log_path
        self._log_handle = None

    def get(self, path: str):
        return http_get(self.base + path)

    def post(self, path: str, body):
        return http_post(self.base + path, body)

    def status(self) -> dict:
        code, body = self.get("/api/status")
        assert code == 200, f"GET /api/status -> {code}: {body!r}"
        return body

    def wait_http_ready(self, timeout: float = 20.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise AssertionError(
                    f"桥进程提前退出 (code={self.proc.returncode}), 日志尾部:\n"
                    + _tail(self.log_path))
            try:
                code, _ = self.get("/api/status")
                if code == 200:
                    return
            except Exception:
                pass
            time.sleep(0.1)
        raise AssertionError(
            f"{timeout}s 内 HTTP 未就绪, 日志尾部:\n" + _tail(self.log_path))

    def wait_phase(self, phase: str, timeout: float = 10.0) -> dict:
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            last = self.status()
            if last.get("phase") == phase:
                return last
            time.sleep(0.05)
        raise AssertionError(f"{timeout}s 内 phase 未到 {phase!r}, 当前 status: {last}")

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                subprocess.run(["taskkill", "/F", "/PID", str(self.proc.pid), "/T"],
                               capture_output=True, check=False)
                try:
                    self.proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
        if self._log_handle:
            try:
                self._log_handle.close()
            except Exception:
                pass


_LOG_DIR = Path(tempfile.mkdtemp(prefix="serialhub_qa_logs_"))
_counter = itertools.count(1)


@pytest.fixture(scope="session", autouse=True)
def _session_hygiene():
    assert BRIDGE_EXE.exists(), (
        f"未找到 {BRIDGE_EXE} —— 请先在项目根执行 cargo build --release")
    kill_all_bridges()   # 清理上次运行可能残留的桥进程
    yield
    kill_all_bridges()   # 会话结束兜底: 不得残留占 COM1 的进程


@pytest.fixture
def start_bridge():
    """桥进程工厂: 每条测试拉起自己的桥, teardown 一律强杀。

    默认 COM1 / 115200 / 8N2; port_name=None 表示不给 --port (FR-5 未打开态);
    extra 附加 CLI 参数 (如 ["--no-open"])。
    """
    started: list[Bridge] = []

    def _start(port_name=BRIDGE_COM, baud=115200, config="8N2",
               extra=None, wait=True) -> Bridge:
        http_port = free_tcp_port()
        # Sprint 2 起二进制默认 GUI 模式; 套件要旧行为 (无窗口), 统一加 --headless
        cmd = [str(BRIDGE_EXE), "--headless"]
        if port_name is not None:
            cmd += ["--port", port_name, "--baud", str(baud), "--config", config]
        cmd += ["--addr", f"{HTTP_HOST}:{http_port}"]
        if extra:
            cmd += extra
        log = _LOG_DIR / f"bridge_{next(_counter)}.log"
        lf = open(log, "w", encoding="utf-8")
        proc = subprocess.Popen(cmd, stdout=lf, stderr=subprocess.STDOUT)
        b = Bridge(proc, http_port, log)
        b._log_handle = lf
        started.append(b)
        if wait:
            b.wait_http_ready()
        return b

    yield _start
    for b in started:
        b.stop()
    kill_all_bridges()   # 双保险: 任何失败路径下也不得残留占 COM1 的进程


# ------------------------------------------------------------------ COM2 对端

@pytest.fixture
def make_peer():
    """pyserial 对端工厂 (占 COM2), teardown 统一关闭。"""
    opened = []

    def _open(baud=115200, bytesize=8, parity="N", stopbits=1,
              timeout=3.0, write_timeout=30.0) -> serial.Serial:
        ser = serial.Serial(port=PEER_COM, baudrate=baud, bytesize=bytesize,
                            parity={"N": serial.PARITY_NONE,
                                    "E": serial.PARITY_EVEN,
                                    "O": serial.PARITY_ODD}[parity],
                            stopbits={1: serial.STOPBITS_ONE,
                                      2: serial.STOPBITS_TWO}[stopbits],
                            timeout=timeout, write_timeout=write_timeout)
        opened.append(ser)
        return ser

    yield _open
    for ser in opened:
        try:
            ser.close()
        except Exception:
            pass


def read_exactly(ser: serial.Serial, n: int, deadline_s: float = 30.0) -> bytes:
    """从对端读满 n 字节; 读不满即失败 (丢字节直接 FAIL, 数量进报错信息)。"""
    buf = bytearray()
    end = time.monotonic() + deadline_s
    while len(buf) < n:
        chunk = ser.read(n - len(buf))
        if chunk:
            buf += chunk
            continue
        if time.monotonic() >= end:
            raise AssertionError(
                f"对端 {PEER_COM} 读超时: 期望 {n}B, 实得 {len(buf)}B (缺 {n - len(buf)}B)")
    return bytes(buf)


def serial_write_all(ser: serial.Serial, payload: bytes) -> None:
    view = memoryview(payload)
    while view:
        n = ser.write(view)
        if n is None:
            n = len(view)
        if n == 0:
            raise TimeoutError(f"{PEER_COM} 写超时 (已写 {len(payload) - len(view)}B)")
        view = view[n:]


# ------------------------------------------------------------------ WS 客户端工具

def ws_send_and_hold(ws_url: str, frames, hold: threading.Event):
    """线程内连接 WS, 发完 frames 后挂住连接 (让主线程同时做串口侧操作)。

    返回 (thread, ready_event, box); box["error"] 非 None 时主线程应抛出。
    """
    ready = threading.Event()
    box: dict = {}

    def _run():
        async def _main():
            async with websockets.connect(ws_url, max_size=None) as ws:
                ready.set()
                for f in frames:
                    await ws.send(f)
                while not hold.is_set():
                    await asyncio.sleep(0.05)
        try:
            asyncio.run(_main())
        except BaseException as e:  # noqa: BLE001 — 异常原样带回主线程
            box["error"] = e

    th = threading.Thread(target=_run, daemon=True)
    th.start()
    return th, ready, box


def start_ws_collectors(ws_url: str, n: int, expect_bytes: int):
    """一个事件循环里开 n 个 WS 收集器, 各自收满 expect_bytes 后返回。

    返回 (thread, ready_events, box); 完成后 box["payloads"] = [bytes] * n。
    """
    ready = [threading.Event() for _ in range(n)]
    box: dict = {}

    def _run():
        async def _one(i: int) -> bytes:
            async with websockets.connect(ws_url, max_size=None) as ws:
                ready[i].set()
                buf = bytearray()
                while len(buf) < expect_bytes:
                    msg = await asyncio.wait_for(ws.recv(), timeout=30)
                    if not isinstance(msg, (bytes, bytearray)):
                        raise AssertionError(
                            f"客户端{i} 收到非二进制帧: {type(msg).__name__} (FR-1 违约)")
                    buf += msg
                return bytes(buf)

        async def _main():
            box["payloads"] = await asyncio.gather(*(_one(i) for i in range(n)))

        try:
            asyncio.run(_main())
        except BaseException as e:  # noqa: BLE001
            box["error"] = e

    th = threading.Thread(target=_run, daemon=True)
    th.start()
    return th, ready, box


def wait_all_ready(ready, box=None, timeout: float = 10.0) -> None:
    deadline = time.monotonic() + timeout
    for ev in ready:
        if not ev.wait(max(0.0, deadline - time.monotonic())):
            if box is not None and box.get("error"):
                raise box["error"]
            raise AssertionError("WS 客户端连接超时")


def join_collectors(th: threading.Thread, box: dict, timeout: float = 60.0):
    th.join(timeout)
    assert not th.is_alive(), "WS 收集器未在限时内收满预期字节"
    if "error" in box:
        raise box["error"]
    return box["payloads"]


# ------------------------------------------------------------------ pytest 配置

def pytest_configure(config):
    config.addinivalue_line(
        "markers", "perf: PERF 基线测试 (spec §5), 失败如实记录、不阻塞 Sprint")
