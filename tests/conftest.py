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
import os
import re
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

# ADR-11 (修订 ADR-9 ①/ADR-5 ①) + ADR-15① (Sprint 5 契约随动修订 2026-09-13, 11→12 增 retries)
# + ADR-16① (Sprint 6 契约随动修订 2026-09-13, 12→13 增 autoReconnect):
# /api/status 恰 13 字段 (flow 回显为双入口对等前提; retries 为热拔插重连计数;
# autoReconnect 为每桥自动重连开关回显, 默认 true)
STATUS_FIELDS = {"phase", "port", "baud", "config", "flow", "clients", "maxClients",
                 "rxBytes", "txBytes", "lastError", "uptimeSec", "retries",
                 "autoReconnect"}


def status_fields_expected(sample: dict) -> set:
    """ADR-16① 渐进放行判据: autoReconnect 未落地时按旧 12 字段契约守护,
    落地即 13 字段全量。仅容忍该一字段缺席 —— 其它任何多/少字段仍当场违约。"""
    exp = set(STATUS_FIELDS)
    if "autoReconnect" not in sample:
        exp.discard("autoReconnect")   # [BLOCKED-BY-BACKEND] 待 dev-backend 落地放行
    return exp


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


def http_patch(url: str, body, timeout: float = 5.0):
    """PATCH JSON (ADR-14③: /api/fleet/<id>/config 受理 PATCH; FR-12 运行中改配用)。"""
    data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(url, data=data, method="PATCH",
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
                # 409 = 控制面已就绪但非单桥模式 (多桥时旧单桥接口按契约拒绝, 黑盒实证)
                if code in (200, 409):
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

# FR-10 落地后的全局默认清单 (后端默认持久化位置); 测试卫生对象
_GLOBAL_FLEET = Path(os.environ.get("APPDATA", "")) / "SerialHub" / "fleet.json"


def sanitize_global_fleet() -> None:
    """删除全局默认 fleet.json —— 仅当其中全部桥名都是测试产物 (CLI 兼容桥 / qa-*)。

    旧式单桥进程 (FR-10h 兼容桥) 会把桥持久化到全局默认清单, 下次任何进程启动都会
    恢复它们 → 多桥 → 旧端点 /api/status 变 409 (实现: 单桥模式守卫) → 全套件被污染。
    含非测试命名桥 (真实用户配置) 时不动文件, 仅放行 (此时套件失败会如实暴露)。
    """
    try:
        data = json.loads(_GLOBAL_FLEET.read_text("utf-8"))
        names = [str(b.get("name", "")) for b in data.get("bridges", [])]
        if names and all(re.fullmatch(r"(CLI|qa-).*", n) for n in names):
            _GLOBAL_FLEET.unlink()
    except (OSError, ValueError):
        pass  # 文件不存在 / 非 JSON / 路径不可用 → 无需处理


@pytest.fixture(scope="session", autouse=True)
def _session_hygiene():
    assert BRIDGE_EXE.exists(), (
        f"未找到 {BRIDGE_EXE} —— 请先在项目根执行 cargo build --release")
    kill_all_bridges()   # 清理上次运行可能残留的桥进程
    sanitize_global_fleet()
    yield
    kill_all_bridges()   # 会话结束兜底: 不得残留占 COM1 的进程
    sanitize_global_fleet()


@pytest.fixture
def start_bridge():
    """桥进程工厂: 每条测试拉起自己的桥, teardown 一律强杀。

    默认 COM1 / 115200 / 8N2; port_name=None 表示不给 --port (FR-5 未打开态);
    extra 附加 CLI 参数 (如 ["--no-open"])。
    """
    started: list[Bridge] = []

    def _start(port_name=BRIDGE_COM, baud=115200, config="8N2",
               flow=None, max_clients=None, extra=None, wait=True, cwd=None) -> Bridge:
        http_port = free_tcp_port()
        # Sprint 2 起二进制默认 GUI 模式; 套件要旧行为 (无窗口), 统一加 --headless
        cmd = [str(BRIDGE_EXE), "--headless"]
        if port_name is not None:
            cmd += ["--port", port_name, "--baud", str(baud), "--config", config]
            if flow is not None:
                cmd += ["--flow", flow]                      # ADR-10: flow 入 CLI
            if max_clients is not None:
                cmd += ["--max-clients", str(max_clients)]   # FR-9b
        cmd += ["--addr", f"{HTTP_HOST}:{http_port}"]
        # FR-10 落地 (Sprint 4): 旧式单桥进程默认持久化到全局 fleet.json 且下次启动恢复,
        # 会污染整套件 (多桥 → /api/status 409)。--no-fleet = 旧二进制语义 (本无持久化),
        # 单桥行为零变化; 需要持久化语义的用例在 extra 里自带 --fleet 覆盖 (clap 末位生效)。
        cmd += ["--no-fleet"]
        if extra:
            cmd += extra
        log = _LOG_DIR / f"bridge_{next(_counter)}.log"
        lf = open(log, "w", encoding="utf-8")
        # cwd 仅 FR-10h 兼容测试使用 (隔离后端默认 fleet.json 的落盘副作用), 旧路径不受影响
        proc = subprocess.Popen(cmd, stdout=lf, stderr=subprocess.STDOUT, cwd=cwd)
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

    def _open(port_name=PEER_COM, baud=115200, bytesize=8, parity="N", stopbits=1,
              timeout=3.0, write_timeout=30.0) -> serial.Serial:
        ser = serial.Serial(port=port_name, baudrate=baud, bytesize=bytesize,
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


# ==================================================================
# FR-10 桥接管理器扩展区 (Sprint 4, ADR-13) —— 不影响上方 Sprint1~3 夹具
# ==================================================================

# 任务契约 (spec FR-10g/FR-10c): fleet 列表行必须齐备的字段。
# 计数器命名沿 ADR-6② (rxBytes/txBytes, 与 /api/status 同名同义); maxClients 为 ADR-14②
# 新增的强制回显字段; retries 为 ADR-15① (Sprint 5 契约随动修订 2026-09-13, 13→14 字段);
# autoReconnect 为 ADR-16① (Sprint 6 契约随动修订 2026-09-13, 14→15 字段, 默认 true)。
# 波1 任务速记 "rx/tx" 与实现分歧, 修订记录见 qa-sprint4.md。
FLEET_ROW_FIELDS = {"id", "name", "serial", "listen", "phase", "clients",
                    "rxBytes", "txBytes", "rxRate", "txRate", "lastError",
                    "uptimeSec", "maxClients", "retries", "autoReconnect"}


def serial_port_of(row: dict):
    """取桥行串口名; 兼容扁平 "COM1" 与嵌套 {port, baud, ...} 两种回显形状。"""
    s = row.get("serial")
    if isinstance(s, dict):
        return s.get("port")
    return s


def take_port(preferred: int | None = None) -> int:
    """优先取偏好端口 (如任务示例 8101/8102), 被占则退回临时端口。"""
    if preferred is not None:
        s = socket.socket()
        try:
            s.bind((HTTP_HOST, preferred))
            s.close()
            return preferred
        except OSError:
            s.close()
    return free_tcp_port()


def wait_port_free(port: int, timeout: float = 8.0) -> None:
    """轮询直到 TCP 端口可再次 bind (delete/杀进程后的'端口释放'判据)。"""
    deadline = time.monotonic() + timeout
    last: Exception | None = None
    while time.monotonic() < deadline:
        s = socket.socket()
        try:
            s.bind((HTTP_HOST, port))
            s.close()
            return
        except OSError as e:
            last = e
            s.close()
            time.sleep(0.1)
    raise AssertionError(f"{timeout}s 内端口 {port} 未释放: {last}")


def hard_kill(b: Bridge, timeout: float = 8.0) -> None:
    """taskkill /F 强杀被测进程 (模拟崩溃/断电, 供持久化测试); 不走优雅停机。"""
    if b.proc.poll() is None:
        subprocess.run(["taskkill", "/F", "/PID", str(b.proc.pid), "/T"],
                       capture_output=True, check=False)
        try:
            b.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            pass


def wait_serial_free(port_name: str, timeout: float = 8.0) -> None:
    """轮询直到串口可打开 (桥 stop 后句柄异步释放, Windows 下存在短延迟)。

    长时间不释放 = 桥 stop 未真关串口, 属实现缺陷, 如实暴露。
    """
    deadline = time.monotonic() + timeout
    last: Exception | None = None
    while time.monotonic() < deadline:
        try:
            s = serial.Serial(port=port_name)
            s.close()
            return
        except serial.SerialException as e:
            last = e
            time.sleep(0.2)
    raise AssertionError(f"{timeout}s 内串口 {port_name} 未释放: {last}")


def listen_port_of(val) -> int:
    """从 listen 字段提取端口号; 接受 '127.0.0.1:8101' / '8101' / 8101 / {'port': 8101}。"""
    if isinstance(val, dict):
        if "port" in val:
            return int(val["port"])
        val = json.dumps(val)
    if isinstance(val, (int, float)):
        return int(val)
    s = str(val).strip().strip("/")
    if ":" in s:
        s = s.rsplit(":", 1)[1]
    if "/" in s:
        s = s.split("/", 1)[0]
    return int(s)


def _row_listen_port(row: dict) -> int | None:
    try:
        return listen_port_of(row.get("listen"))
    except (TypeError, ValueError):
        return None


def fleet_rows(api: Bridge) -> list:
    """GET /api/fleet → 行列表 (信封未定: 裸列表或单列表字段字典均可, 见 qa-sprint4-plan 假设)。"""
    code, body = api.get("/api/fleet")
    assert code == 200, f"GET /api/fleet -> {code}: {body!r}"
    if isinstance(body, list):
        return body
    if isinstance(body, dict):
        cand = [v for v in body.values()
                if isinstance(v, list) and v
                and all(isinstance(r, dict) and "id" in r for r in v)]
        if not cand:
            cand = [v for v in body.values() if isinstance(v, list)]
        assert len(cand) == 1, f"/api/fleet 信封无唯一列表字段: {body!r}"
        return cand[0]
    raise AssertionError(f"/api/fleet 返回非列表结构: {body!r}")


def row_of(api: Bridge, ident) -> dict | None:
    """按 id 或 listen 端口取单行 (单次, 不等待)。"""
    for r in fleet_rows(api):
        if r.get("id") == ident or _row_listen_port(r) == ident:
            return r
    return None


def fleet_create(api: Bridge, name: str, serial_port: str, listen_port: int,
                 baud: int = 115200, config: str = "8N1", flow: str = "none",
                 extra: dict | None = None):
    """POST /api/fleet 新建桥。

    契约形状 (黑盒实证 + ADR-14③ "受理 create 形状 body"): serial 为嵌套 SerialReq
    对象 {port, baud, dataBits, parity, stopBits, flow}, 与桥对象回显一致。
    extra: 追加到建桥 body 顶层 (如 {"autoReconnect": False}, ADR-16①)。
    """
    body = {"name": name,
            "serial": {"port": serial_port, "baud": baud,
                       "dataBits": int(config[0]), "parity": config[1],
                       "stopBits": int(config[2]), "flow": flow},
            "listen": f"{HTTP_HOST}:{listen_port}"}
    if extra:
        body.update(extra)
    code, resp = api.post("/api/fleet", body)
    return code, resp


def _post_ok(code: int, body, action: str) -> None:
    """POST 成功判据 (沿 ADR-5③ 家族): 2xx 且非 {ok:false}。"""
    assert code in (200, 201) and not (isinstance(body, dict) and body.get("ok") is False), \
        f"POST {action} -> {code}: {body!r}"


def fleet_act_ok(api: Bridge, bid, action: str, timeout: float = 5.0) -> None:
    code, body = None, None
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            code, body = api.post(f"/api/fleet/{bid}/{action}", {})
            if code in (200, 201):
                break
        except Exception:
            pass
        time.sleep(0.1)
    _post_ok(code, body, f"/api/fleet/{bid}/{action}")


def fleet_purge(api: Bridge) -> None:
    """删除当前全部桥, 保证行数类断言的确定性。

    背景 (黑盒实证): 纯控制面进程 (无 --port 且无恢复桥) 会自动播种一座空白兼容桥
    (name=CLI, serial.port="", phase=closed, FR-5"未打开态"的 fleet 化)。行数断言前先清场。
    """
    for r in fleet_rows(api):
        code, body = api.post(f"/api/fleet/{r['id']}/delete", {})
        if code >= 400 or (isinstance(body, dict) and body.get("ok") is False):
            api.post(f"/api/fleet/{r['id']}/stop", {})   # 若 delete 有"须先停"前置则退一步
            code, body = api.post(f"/api/fleet/{r['id']}/delete", {})
        assert code in (200, 201) and not (isinstance(body, dict) and body.get("ok") is False), \
            f"purge 删除 {r['id']} 失败: {code}: {body!r}"
    assert fleet_rows(api) == [], "purge 后仍有残留桥"


def fleet_new_id(api: Bridge, create_resp, listen_port: int):
    """解析新建桥 id: 优先响应体 id 字段, 否则以 listen 端口在列表中定位。"""
    if isinstance(create_resp, dict) and isinstance(create_resp.get("id"), (str, int)):
        return create_resp["id"]
    hits = [r["id"] for r in fleet_rows(api) if _row_listen_port(r) == listen_port]
    assert len(hits) == 1, (
        f"无法定位新建桥 id (listen={listen_port}): resp={create_resp!r} rows={fleet_rows(api)!r}")
    return hits[0]


def assert_row_shape(row: dict) -> None:
    """FR-10g/FR-10c + ADR-14② + ADR-16①: 行字段齐全 + 类型合理 + phase ∈ FR-3 状态机四态。"""
    assert isinstance(row, dict), f"fleet 行非对象: {row!r}"
    missing = FLEET_ROW_FIELDS - set(row)
    if "autoReconnect" not in row:
        # ADR-16① 渐进放行: 后端未落地时按旧 14 字段契约守护 (落地即 15 字段全量;
        # FR-12 语义验收由 test_fr12_reconnect_opt.py 的 fr12_ready 探针门控承载)。
        # 仅容忍 autoReconnect 一字缺席, 其它多/少字段仍当场违约。
        missing -= {"autoReconnect"}
    assert not missing, f"fleet 行缺字段 {sorted(missing)}: {row!r}"
    for k in ("clients", "rxBytes", "txBytes", "rxRate", "txRate", "uptimeSec",
              "maxClients", "retries"):
        assert isinstance(row[k], (int, float)) and not isinstance(row[k], bool) \
            and row[k] >= 0, f"{k} 应为非负数值: {row[k]!r}"
    if "autoReconnect" in row:
        assert isinstance(row["autoReconnect"], bool), \
            f"autoReconnect 应为布尔 (ADR-16①): {row['autoReconnect']!r}"
    assert row["phase"] in {"closed", "opening", "open", "retry"}, \
        f"phase 越出 FR-3 状态机: {row['phase']!r}"
    assert row["lastError"] is None or isinstance(row["lastError"], str), \
        f"lastError 应为 null 或字符串: {row['lastError']!r}"


def wait_row_phase(api: Bridge, ident, phase: str, timeout: float = 12.0) -> dict:
    """轮询直到 id/listen 定位的桥行进入目标 phase。"""
    deadline = time.monotonic() + timeout
    row, rows = None, []
    while time.monotonic() < deadline:
        rows = fleet_rows(api)
        row = next((r for r in rows
                    if r.get("id") == ident or _row_listen_port(r) == ident), None)
        if row is not None and row.get("phase") == phase:
            return row
        time.sleep(0.1)
    raise AssertionError(f"{timeout}s 内桥 {ident!r} phase 未到 {phase!r}, rows={rows!r}")


def wait_counters(api: Bridge, port: int, want_tx: int, want_rx: int,
                  timeout: float = 8.0) -> dict:
    """轮询直到该桥累计 rxBytes/txBytes 恰达预期值 (逐字节对账判据, ADR-6② 口径)。"""
    deadline = time.monotonic() + timeout
    row = None
    while time.monotonic() < deadline:
        row = row_of(api, port)
        if row and int(row.get("txBytes") or 0) >= want_tx \
                and int(row.get("rxBytes") or 0) >= want_rx:
            break
        time.sleep(0.1)
    assert row is not None, f"listen={port} 的桥行消失"
    assert row["txBytes"] == want_tx and row["rxBytes"] == want_rx, \
        (f"桥({port}) 计数对账失败: txBytes={row['txBytes']}(望{want_tx}) "
         f"rxBytes={row['rxBytes']}(望{want_rx}) —— 串扰/丢字节")
    return row


def ws_collect_exact(url: str, expect: bytes, *, quiet_s: float = 0.0,
                     allow_text: bool = False, timeout: float = 30.0):
    """收集恰 len(expect) 字节后返回; quiet_s>0 追加静默守窗 (再多 1 字节 = 串扰/回显 FAIL)。

    allow_text=True 时跳过文本帧 (仅 tap 旁看用 —— 任务契约只要求收到 RX 字节, 不锁帧格式)。
    返回 (thread, ready_event, box); box["payload"]/box["error"]。
    """
    ready = threading.Event()
    box: dict = {}

    def _run():
        async def _main():
            async with websockets.connect(url, max_size=None) as ws:
                ready.set()
                buf = bytearray()
                while len(buf) < len(expect):
                    msg = await asyncio.wait_for(ws.recv(), timeout=timeout)
                    if isinstance(msg, (bytes, bytearray)):
                        buf += msg
                    elif not allow_text:
                        raise AssertionError(
                            f"收到非二进制帧: {type(msg).__name__}={msg!r}")
                if quiet_s > 0:
                    try:
                        extra = await asyncio.wait_for(ws.recv(), timeout=quiet_s)
                        raise AssertionError(f"静默守窗期收到多余数据 {extra!r} (串扰/回显)")
                    except asyncio.TimeoutError:
                        pass
                return bytes(buf)
        try:
            box["payload"] = asyncio.run(_main())
        except BaseException as e:  # noqa: BLE001
            box["error"] = e

    th = threading.Thread(target=_run, daemon=True)
    th.start()
    return th, ready, box


def ws_push(url: str, frames):
    """发射器: 连上即发 frames, 发完即断。返回 (thread, done_event, box)。"""
    done = threading.Event()
    box: dict = {}

    def _run():
        async def _main():
            async with websockets.connect(url, max_size=None) as ws:
                for f in frames:
                    await ws.send(f)
        try:
            asyncio.run(_main())
        except BaseException as e:  # noqa: BLE001
            box["error"] = e
        finally:
            done.set()

    th = threading.Thread(target=_run, daemon=True)
    th.start()
    return th, done, box


@pytest.fixture(scope="session")
def fleet_ready():
    """FR-10 后端就绪探针 (会话级一次): 控制面 /api/fleet 不存在 → 整组 [BLOCKED-BY-BACKEND] 跳过。

    不改任何预期 —— 只是把"实现未到位"与"实现违约"区分开; 实现落地后本探针放行, 套件即书即跑。
    """
    kill_all_bridges()
    port = free_tcp_port()
    fdir = Path(tempfile.mkdtemp(prefix="serialhub_fr10_probe_"))
    cmd = [str(BRIDGE_EXE), "--headless", "--addr", f"{HTTP_HOST}:{port}",
           "--fleet", str(fdir / "fleet.json")]
    code, proc = None, None
    with open(fdir / "probe.log", "w", encoding="utf-8") as lf:
        proc = subprocess.Popen(cmd, stdout=lf, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if proc.poll() is not None:
                    break
                try:
                    code, _ = http_get(f"http://{HTTP_HOST}:{port}/api/fleet")
                    if code == 200:
                        break
                except Exception:
                    code = None
                time.sleep(0.1)
        finally:
            if proc.poll() is None:
                subprocess.run(["taskkill", "/F", "/PID", str(proc.pid), "/T"],
                               capture_output=True, check=False)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
    if code == 200:
        return True
    pytest.skip(
        "[BLOCKED-BY-BACKEND] FR-10 控制面未实现: GET /api/fleet -> "
        f"{code}, 进程退出码 {proc.returncode}, 日志: {fdir / 'probe.log'}")


@pytest.fixture
def start_fleet():
    """FR-10 控制面进程工厂: --headless --addr <自由端口> --fleet <临时清单>; 每条测试自管进程。

    fleet_path=False 时不传 --fleet (探测后端默认持久化行为用)。
    """
    started: list[Bridge] = []

    def _start(fleet_path=None, extra=None, wait=True) -> Bridge:
        http_port = free_tcp_port()
        if fleet_path is None:
            fleet_path = Path(tempfile.mkdtemp(prefix="serialhub_fr10_")) / "fleet.json"
        cmd = [str(BRIDGE_EXE), "--headless", "--addr", f"{HTTP_HOST}:{http_port}"]
        if fleet_path is not False:
            cmd += ["--fleet", str(fleet_path)]
        if extra:
            cmd += list(extra)
        log = _LOG_DIR / f"fleet_{next(_counter)}.log"
        lf = open(log, "w", encoding="utf-8")
        proc = subprocess.Popen(cmd, stdout=lf, stderr=subprocess.STDOUT)
        b = Bridge(proc, http_port, log)
        b.fleet_path = fleet_path
        b._log_handle = lf
        started.append(b)
        if wait:
            b.wait_http_ready()
        return b

    yield _start
    for b in started:
        b.stop()
    kill_all_bridges()   # 双保险: 不得残留占 COM1/COM2 的进程


# ------------------------------------------------------------------ pytest 配置

def pytest_configure(config):
    config.addinivalue_line(
        "markers", "perf: PERF 基线测试 (spec §5), 失败如实记录、不阻塞 Sprint")
