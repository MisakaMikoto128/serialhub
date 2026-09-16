# -*- coding: utf-8 -*-
"""Sprint 14 QA 随动 — FR-16 单实例友好处理 (ADR-25 用户裁定版)。

契约 (spec FR-16 ADR-25 改版 + 任务契约, 黑盒只认可观察行为):
- 第二实例 bind 失败时探测目标端口 /api/status (响应含 "phase" 判为另一 SerialHub):
  * 是 SerialHub → 第二实例向目标 POST /api/show 令**已运行实例把主窗口拉到前台**
    (对端 headless 无窗口时 shown=false 同样算成功), 随后**静默以 0 退出**:
    无提示框、不开浏览器 —— "重复点图标 = 把软件叫回来";
    headless 第二实例: stderr 一行 (含 "已在运行") + 0 退出;
  * 非 SerialHub 占用 → 维持旧错误路径: headless 退出码非 0 且报占用语义,
    GUI 维持旧错误框 (FR-8), 退出码非 0。
- 第一实例不受影响 (仍活着, /api/status 仍 200)。
- **实例 A 主窗口被前置** (GUI 双实例, Win32 断言): A 关窗到托盘
  (IsWindowVisible=false) → 第二实例退出后 A 的窗口重新可见
  (IsWindowVisible=true 且 IsIconic=false)。

构建矩阵与 FR-18 (ADR-19③: release 为 GUI 子系统, headless release 的 stdout/stderr
不可见属既定取舍):
- **debug 构建**: 断退出码 + 断 stderr 文本;
- **release 构建**: 只断退出码 (文本不可见, 不断言)。
两种构建都跑 (release = 用户实际拿到的东西)。

门控 (沿 fleet_ready/fr12_ready 先例, 区分"未落地"与"违约"):
- `fr16_gate` 会话级行为探针: 对每个构建实测 "第二实例 exit==0?"。
  探针不过 → 对应组 SKIPPED [BLOCKED-BY-BACKEND], dev-backend 落地即自动放行。
  非 SerialHub 占用组不设门 —— 基线即 exit 1 + 报占用 (2026-09-13 实测),
  落地前后都必须成立 (守护"旧错误路径不被误伤")。

GUI 进程级说明 (ADR-25): 第二实例**不得**弹出 MessageBox —— 自动化在 B 存活期
轮询 #32770, 一经出现即判违约 (弹框会阻塞退出, 检出可靠); "确定无浏览器标签被
打开 / A 窗口抢到前台焦点 (GetForegroundWindow)" 属目视项, 见人工清单。
无显示环境可设 SERIALHUB_QA_SKIP_GUI=1 跳过 GUI 用例。

端口/串口: 管理台端口优先 8095/8096 (被占退回自由端口); **本模块不占任何串口**
(探测只依赖控制面 /api/status 含 phase, 纯控制面进程即可提供), 减少串口争用。

运行: 项目根执行  python -m pytest tests/test_fr16_single_instance.py -v
"""
from __future__ import annotations

import ctypes
import ctypes.wintypes as wt
import os
import socket
import subprocess
import time
import urllib.error

import pytest

from conftest import (BRIDGE_EXE, HTTP_HOST, PROJECT_ROOT, http_get,
                      kill_all_bridges, take_port)

DEBUG_EXE = PROJECT_ROOT / "target" / "debug" / "serialhub.exe"
BUILD_PARAMS = [
    pytest.param(BRIDGE_EXE, id="release"),   # 用户实际拿到的东西 (FR-18: 无 stderr)
    pytest.param(DEBUG_EXE, id="debug"),      # stderr 可见, 断文本
]

# ADR-25 契约文案的判别子串 (headless stderr: "SerialHub 已在运行, 已唤起主窗口"
# / "已在运行, 对端无窗口可前置" 等; 取宽松核心 "已在运行", 唤起结果分档的前后缀
# 不锁, 避免过度约束实现)
FRIENDLY_MARK = "已在运行"
OCCUPIED_MARK = "占用"


# ------------------------------------------------------------------ 进程管理

def addr_args(port: int):
    return ["--addr", f"{HTTP_HOST}:{port}", "--no-fleet"]


class Instance:
    """一个 serialhub 进程 (任一构建/任一形态) + 最小观测面。"""

    def __init__(self, exe, args):
        self.exe = exe
        cmd = [str(exe)] + args
        # CREATE_NO_WINDOW: debug 构建是控制台子系统, 防测试机闪黑窗; 对 GUI 形态无影响
        flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        self.proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.PIPE,
                                     creationflags=flags)

    @property
    def alive(self):
        return self.proc.poll() is None

    def status(self, port: int):
        return http_get(f"http://{HTTP_HOST}:{port}/api/status")

    def stderr_text(self, timeout=20.0):
        """等进程退出并取 stderr (utf-8, 容错); 超时强杀防挂死。"""
        try:
            _, err = self.proc.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            kill_pid(self.proc)
            raise AssertionError(
                f"第二实例 {timeout}s 未退出 (FR-16 应快速探测并退出)")
        return (err or b"").decode("utf-8", "replace")

    def stop(self):
        if self.alive:
            kill_pid(self.proc)
        try:
            self.proc.stderr.close()
        except Exception:
            pass


def kill_pid(proc: subprocess.Popen):
    subprocess.run(["taskkill", "/F", "/PID", str(proc.pid), "/T"],
                   capture_output=True, check=False)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass


def start_first_instance(exe, http_port):
    """第一实例 A: headless 纯控制面 (无 --port, 自动播种 CLI 兼容桥)。

    /api/status 200 且含 "phase" —— FR-16 探测判别的充分条件, 不需要串口。
    """
    a = Instance(exe, ["--headless"] + addr_args(http_port))
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if not a.alive:
            raise AssertionError("第一实例 A 提前退出 (端口冲突?)")
        try:
            code, body = a.status(http_port)
            if code == 200 and isinstance(body, dict) and "phase" in body:
                return a
        except Exception:
            pass
        time.sleep(0.1)
    a.stop()
    raise AssertionError("15s 内第一实例 A 的 /api/status 未就绪")


@pytest.fixture
def instances():
    """每条测试的进程生命周期: teardown 全杀 + kill_all_bridges 双保险 (绝不残留)。"""
    procs: list[Instance] = []
    yield procs
    for p in procs:
        p.stop()
    kill_all_bridges()


# ------------------------------------------------------------------ 门控探针

def _probe_friendly_exit0(exe) -> bool:
    """行为探针: 第一实例 A + 第二实例 headless 同地址 → B 是否 exit 0。"""
    port = take_port(8095)
    a = start_first_instance(exe, port)
    try:
        b = Instance(exe, ["--headless"] + addr_args(port))
        try:
            b.stderr_text(timeout=15)
            return b.proc.returncode == 0
        finally:
            b.stop()
    finally:
        a.stop()


@pytest.fixture(scope="session")
def fr16_gate():
    """会话级一次, 每构建探针缓存: True=FR-16 已落地 / False=未落地 / None=无该构建。"""
    gate = {}
    for name, exe in (("release", BRIDGE_EXE), ("debug", DEBUG_EXE)):
        if not exe.exists():
            gate[name] = None
            continue
        try:
            gate[name] = _probe_friendly_exit0(exe)
        except Exception as e:   # 探针自身故障 (环境问题) 不伪装成"未落地"
            gate[name] = False
            gate[f"{name}_err"] = repr(e)
    return gate


def _skip_unless_ready(gate, name):
    if gate[name] is None:
        pytest.skip(f"[BLOCKED-BY-BACKEND] {name} 构建不存在: "
                    f"请先 cargo build{' --release' if name == 'release' else ''}")
    if not gate[name]:
        extra = gate.get(f"{name}_err", "")
        pytest.skip(
            "[BLOCKED-BY-BACKEND] FR-16 未落地 (行为探针: 第二实例 headless 未以 0 退出"
            f"{' — ' + extra if extra else ''}); dev-backend 落地即自动放行")


# ------------------------------------------------------------------ FR-16 用例

@pytest.mark.parametrize("exe", BUILD_PARAMS)
def test_fr16a_headless_second_instance_friendly(exe, fr16_gate, instances):
    """headless 第二实例 (目标=另一 SerialHub): exit 0 + stderr 含"已在运行"; A 仍活。

    ADR-25: 第二实例 POST /api/show 唤起对端 (对端 headless → shown=false,
    同样算成功), stderr 一行后静默 0 退出。
    release 构建 (FR-18) 无 stderr, 只断 exit 0; debug 断文本。
    """
    _skip_unless_ready(fr16_gate, "release" if exe is BRIDGE_EXE else "debug")
    port = take_port(8095)
    a = start_first_instance(exe, port)
    instances.append(a)

    b = Instance(exe, ["--headless"] + addr_args(port))
    instances.append(b)
    err = b.stderr_text(timeout=15)

    assert b.proc.returncode == 0, (
        f"FR-16 违约: headless 第二实例应 0 退出, 实得 {b.proc.returncode}; "
        f"stderr={err!r}")
    if exe is DEBUG_EXE:   # FR-18: release 无 stderr, 文本只在 debug 断
        assert FRIENDLY_MARK in err, (
            f"FR-16 违约: headless 第二实例 stderr 应含 {FRIENDLY_MARK!r}, 实得 {err!r}")
    # 第一实例不受影响
    assert a.alive, "第二实例退出后第一实例 A 竟退出 (不得误伤)"
    code, body = a.status(port)
    assert code == 200 and "phase" in body, "第二实例退出后 A 的 /api/status 不再 200"


@pytest.mark.parametrize("exe", BUILD_PARAMS)
def test_fr16b_headless_non_serialhub_occupant(exe, instances):
    """非 SerialHub 占用 (裸 socket): headless 第二实例 exit 非 0 + 报占用语义。

    不设门: v1.3.0 基线即成立 (2026-09-13 实测 exit 1 + "管理台端口被占用"),
    FR-16 落地后必须维持 —— 守护"旧错误路径不被友好化误伤"。
    release 只断退出码 (FR-18 无 stderr)。
    """
    port = take_port(8096)
    occupier = socket.socket()
    occupier.bind((HTTP_HOST, port))
    occupier.listen(1)
    try:
        b = Instance(exe, ["--headless", "--addr", f"{HTTP_HOST}:{port}",
                           "--no-fleet"])
        instances.append(b)
        err = b.stderr_text(timeout=15)
        assert b.proc.returncode != 0, (
            f"FR-16 违约: 非 SerialHub 占用必须维持旧错误路径 (exit 非 0), "
            f"实得 {b.proc.returncode}; stderr={err!r}")
        if exe is DEBUG_EXE:   # FR-18: release 无 stderr, 文本只在 debug 断
            assert OCCUPIED_MARK in err, (
                f"FR-16 违约: 占用报错应含 {OCCUPIED_MARK!r} 语义, 实得 {err!r}")
    finally:
        occupier.close()


@pytest.mark.parametrize("exe", BUILD_PARAMS)
def test_fr16c_gui_second_instance_wakes_first_window(exe, fr16_gate, instances):
    """GUI 第二实例 (目标=另一 GUI SerialHub): 静默 exit 0 + 唤回 A 的主窗口。

    ADR-25 改版 (原"信息框+开浏览器"废除): 第二实例无提示框、不开浏览器,
    POST /api/show 把已运行实例的主窗口拉到前台后 0 退出。
    Win32 断言: A 主窗口先经 WM_CLOSE 关窗到托盘 (IsWindowVisible=false),
    第二实例退出后重新可见 (IsWindowVisible=true 且 IsIconic=false)。
    自动化在 B 存活期轮询 #32770, 一经出现即判违约 (弹框阻塞退出, 检出可靠);
    "无浏览器标签被打开 / 焦点抢到前台" 属目视项 (人工清单)。
    无显示环境设 SERIALHUB_QA_SKIP_GUI=1 跳过。
    """
    if os.environ.get("SERIALHUB_QA_SKIP_GUI"):
        pytest.skip("SERIALHUB_QA_SKIP_GUI=1 (无显示环境, GUI 用例人工执行)")
    _skip_unless_ready(fr16_gate, "release" if exe is BRIDGE_EXE else "debug")
    port = take_port(8095)
    a = Instance(exe, addr_args(port))   # 第一实例 = GUI 形态 (有主窗口)
    instances.append(a)
    hwnd = find_main_window_of_pid(a.proc.pid, timeout=25.0)
    assert hwnd, "实例 A 的主窗口 25s 内未出现 (标题 SerialHub)"
    # 关窗到托盘 (与用户点 X 同一 WM_CLOSE 路径) → 主窗口隐藏
    ctypes.windll.user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
    assert _wait(lambda: not _is_visible(hwnd), 5.0), "A 主窗口关窗后未隐藏"

    # 第二实例 B: 同地址 GUI → 应无弹框、静默 0 退出, 且唤回 A 的窗口
    b = Instance(exe, addr_args(port))
    instances.append(b)
    deadline = time.monotonic() + 25
    while time.monotonic() < deadline:
        if find_msgbox_of_pid(b.proc.pid):
            raise AssertionError(
                "FR-16/ADR-25 违约: 第二实例弹出了对话框 (应无提示框静默退出)")
        if not b.alive:
            break
        time.sleep(0.2)
    assert not b.alive, "GUI 第二实例 25s 内未退出 (ADR-25 应快速静默退出)"
    assert b.proc.returncode == 0, (
        f"FR-16/ADR-25 违约: GUI 第二实例应 0 退出, 实得 {b.proc.returncode}")
    assert not find_msgbox_of_pid(b.proc.pid), (
        "FR-16/ADR-25 违约: 第二实例退出前存在消息框 (应无提示框)")

    # A 的主窗口被唤回: 重新可见且非最小化 (可见性 隐藏→可见 分支)
    assert _wait(lambda: _is_visible(hwnd) and not _is_iconic(hwnd), 10.0), (
        "A 的主窗口未被唤回前台 (IsWindowVisible 应 true 且 IsIconic 应 false)")
    assert a.alive, "第二实例唤起后第一实例 A 竟退出 (不得误伤)"
    code, body = a.status(port)
    assert code == 200 and "phase" in body, "第二实例退出后 A 的 /api/status 不再 200"


@pytest.mark.parametrize("exe", BUILD_PARAMS)
def test_fr16d_gui_non_serialhub_occupant_keeps_error_box(exe, instances):
    """非 SerialHub 占用时 GUI 第二实例: 维持旧错误框 (FR-8), exit 非 0。

    不设门 (落地前后行为都须如此): 错误框仍以 WM_CLOSE 关闭, 进程随后非 0 退出。
    错误框样式/文案目视项见人工清单。
    """
    if os.environ.get("SERIALHUB_QA_SKIP_GUI"):
        pytest.skip("SERIALHUB_QA_SKIP_GUI=1 (无显示环境, GUI 用例人工执行)")
    port = take_port(8096)
    occupier = socket.socket()
    occupier.bind((HTTP_HOST, port))
    occupier.listen(1)
    try:
        gui = Instance(exe, ["--addr", f"{HTTP_HOST}:{port}", "--no-fleet"])
        instances.append(gui)
        dismiss_msgbox_of_pid(gui.proc.pid, timeout=20.0)
        deadline = time.monotonic() + 20
        while gui.alive and time.monotonic() < deadline:
            time.sleep(0.2)
        assert not gui.alive, "GUI 第二实例确定错误框后 20s 内未退出"
        assert gui.proc.returncode != 0, (
            f"FR-16 违约: 非 SerialHub 占用须维持旧错误路径 (exit 非 0), "
            f"实得 {gui.proc.returncode}")
    finally:
        occupier.close()


# ------------------------------------------------------------------ Win32 辅助

WM_CLOSE = 0x0010


def _enum_windows_of_pid(pid: int):
    """枚举 pid 拥有的全部可见顶层窗口 hwnd (单次枚举)。"""
    if os.name != "nt":
        return []
    user32 = ctypes.windll.user32
    ENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_bool, wt.HWND, wt.LPARAM)
    hits = []

    def _cb(hwnd, _lparam):
        if user32.IsWindowVisible(hwnd):
            owner = wt.DWORD()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
            if owner.value == pid:
                hits.append(hwnd)
        return True

    user32.EnumWindows(ENUMPROC(_cb), 0)
    return hits


def find_msgbox_of_pid(pid: int):
    """找 pid 的可见 MessageBox (对话框类 #32770), 返回 hwnd 或 None。

    ADR-25 语义下用于**负向断言** (第二实例不得弹框): 弹框会阻塞进程退出,
    轮询检出的可靠性由"框活着 ⇒ 进程活着"保证。
    """
    for hwnd in _enum_windows_of_pid(pid):
        buf = ctypes.create_unicode_buffer(64)
        ctypes.windll.user32.GetClassNameW(hwnd, buf, 64)
        if buf.value == "#32770":
            return hwnd
    return None


def find_main_window_of_pid(pid: int, title_sub: str = "SerialHub",
                            timeout: float = 20.0):
    """等 pid 的 SerialHub 主窗口出现 (按窗口标题, 排除 #32770 对话框)。

    轮询等待: GUI 进程先起服务后建窗, 需要兜住启动时延; 超时返回 None。
    """
    if os.name != "nt":
        pytest.skip("主窗口查找辅助仅 Windows")
    user32 = ctypes.windll.user32
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for hwnd in _enum_windows_of_pid(pid):
            buf = ctypes.create_unicode_buffer(256)
            user32.GetWindowTextW(hwnd, buf, 256)
            cls = ctypes.create_unicode_buffer(64)
            user32.GetClassNameW(hwnd, cls, 64)
            if title_sub in buf.value and cls.value != "#32770":
                return hwnd
        time.sleep(0.2)
    return None


def _is_visible(hwnd) -> bool:
    return bool(ctypes.windll.user32.IsWindowVisible(hwnd))


def _is_iconic(hwnd) -> bool:
    return bool(ctypes.windll.user32.IsIconic(hwnd))


def _wait(predicate, timeout: float) -> bool:
    """predicate 在 timeout 内成立 → True (ADR-25 唤回观测窗口)。"""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.2)
    return False


def dismiss_msgbox_of_pid(pid: int, timeout: float = 20.0) -> None:
    """等 pid 的可见 MessageBox 出现并以 WM_CLOSE 关闭 (错误框用例仍需, FR-8)。

    单键 (MB_OK) 消息框下 WM_CLOSE 等价点"确定"; 找不到/超时 = 弹窗未出现, 判失败。
    """
    if os.name != "nt":
        pytest.skip("MessageBox 关闭辅助仅 Windows")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        hwnd = find_msgbox_of_pid(pid)
        if hwnd:
            ctypes.windll.user32.PostMessageW(hwnd, WM_CLOSE, 0, 0)
            return
        time.sleep(0.2)
    raise AssertionError(
        f"{timeout}s 内未出现 pid={pid} 的消息框 (信息框/错误框未弹出或类名不符)")
