# -*- coding: utf-8 -*-
"""配置/持久化交叉矩阵黑盒套件 — 测试二组 (配置与持久化方向) / 2026-09-15。

补 qa-sprint11 (ADR-22① 窗口记忆) 与 qa-sprint13 (FR-19/22 录像与导入导出) 单项验收
之间的**交叉盲区**。预期只来自 spec 条目交互 + ADR-22① 归属裁定, 不读 src/ 调预期:

1  导入 × 运行态: 桥 A 运行中 → replace 导入含桥 B 的清单 → A 被停/替换 (行消失 +
   串口释放), B 按清单启动 (running + autoOpen 开串口); 再 merge 导入含 A 的清单 →
   A 回来且 B 不受影响 (FR-22 × FR-10g 运行态语义)。
2  导入 × 窗口记忆: replace/merge 导入后 fleet.json 顶层 [window] 段必须保留 ——
   导入清单不含 window 键 (他机清单常态), ADR-22①「window 属本机状态, fleet.json
   是单一事实来源」× FR-22「导入整表替换」的交互风险点 (同 dev-sprint11 BUG-1
   「整份重写抹掉 window」的导入路径变体)。
3  导入 × 录像文件: replace 导入移除旧桥后, 其 recordings/ 文件必须原样保留
   (FR-19「桥删除录像保留」× FR-22「replace 先停后删旧桥」); COM1 空闲时用真 open 桥
   灌 WS TX 载荷 (非空录像), 被占则降级 COM99 retry 桥 (文件仍为本桥录制产出)。
4  单实例 × fleet: 实例 A (有桥) 运行中, 第二实例 headless 指同管理台端口+同清单 →
   友好退出 (FR-16, exit 0) 且 A 的 fleet.json 逐字节不变 (window/bridges 段不被
   第二实例写坏)。
5  主题持久化 × 重启: 真 Chromium (Playwright 持久化 profile) 走 UI 设置切 win95 →
   **重启浏览器进程** (同 profile) → 页面 head 脚本按 localStorage 记忆挂回 win95
   (FR-14「选择记忆在浏览器本地」)。壳 webview (wry) 的 localStorage 跨重启持久性
   无法黑盒自动化, 沿 qa-sprint11 §3 留用户人工项。

门控: fr22_ready / fr19_ready / fleet_ready (conftest 会话探针, 区分未落地与违约);
COM1/COM2 被并行席位占用时串口用例 [BLOCKED-BY-ENVIRONMENT] 跳过 (占着不硬撞);
Playwright 不可用 → [BLOCKED-BY-TOOLING]。

环境纪律: COM1(桥)↔COM2(对端/桥B), 全程禁碰 COM8; 管理台/数据口取 18400+;
独立构建 target-f2 (SERIALHUB_EXE 指入) —— recordings/ 落 target-f2/release/recordings,
与并行席位 (output/ui-xtest, 18500+) 互不干扰; 进程按 PID 清理, 不 commit。

运行: 项目根执行
  SERIALHUB_EXE=target-f2/release/serialhub.exe python -m pytest tests/test_xtest_config.py -v
"""
from __future__ import annotations

import copy
import itertools
import json
import os
import subprocess
import tempfile
import threading
import time
from pathlib import Path

import pytest
import serial

from conftest import (
    BRIDGE_COM,
    BRIDGE_EXE,
    HTTP_HOST,
    PEER_COM,
    PROJECT_ROOT,
    Bridge,
    delete_recordings,
    fleet_act_ok,
    fleet_create,
    fleet_new_id,
    fleet_purge,
    fleet_rows,
    http_get,
    list_recordings,
    listen_port_of,
    post_accepted,
    recordings_dir,
    row_of,
    serial_port_of,
    take_port,
    wait_port_free,
    wait_row_phase,
    ws_push,
)

PW_SCRIPT = Path(__file__).resolve().parent / "pw_theme_persist.cjs"
PW_TIMEOUT_S = 120.0          # node 单次上限 (浏览器冷启动余量, 同 fr17 口径放宽)

# ADR-22①: window 段 {x,y,w,h,maximized}; 种子值任意合法即可, 断言「原样保留」而非具体值
WINDOW_SEED = {"x": 120, "y": 80, "w": 1024, "h": 768, "maximized": False}


# ------------------------------------------------------------------ 进程工厂

_LOG_DIR = Path(tempfile.mkdtemp(prefix="serialhub_xtest_logs_"))
_SEQ = itertools.count(1)


def _spawn(port: int, *, fleet_path=None, extra=None, wait=True) -> Bridge:
    """控制面进程工厂 (本地版, 不动 conftest 公共夹具): headless + --addr(18400+) + --fleet。

    fleet_path=None 时落临时目录; cwd=工程根 (fr13 先例, 主题/资源按工程根+exe 旁解析)。
    """
    if fleet_path is None:
        fleet_path = Path(tempfile.mkdtemp(prefix="serialhub_xtest_")) / "fleet.json"
    cmd = [str(BRIDGE_EXE), "--headless", "--addr", f"{HTTP_HOST}:{port}"]
    cmd += ["--fleet", str(fleet_path)]
    if extra:
        cmd += list(extra)
    log = _LOG_DIR / f"xt_{next(_SEQ)}.log"
    lf = open(log, "w", encoding="utf-8")
    proc = subprocess.Popen(cmd, stdout=lf, stderr=subprocess.STDOUT, cwd=str(PROJECT_ROOT))
    b = Bridge(proc, port, log)
    b._log_handle = lf
    b.fleet_path = Path(fleet_path)
    if wait:
        b.wait_http_ready()
    return b


@pytest.fixture
def spawn_fleet():
    started: list[Bridge] = []

    def _start(port=None, **kw) -> Bridge:
        b = _spawn(port if port is not None else take_port(18400), **kw)
        started.append(b)
        return b

    yield _start
    for b in started:
        b.stop()
    from conftest import kill_all_bridges
    kill_all_bridges()   # 双保险: 任何失败路径不残留占 COM1/COM2 的进程


@pytest.fixture
def serial_pair_free():
    """串口占用门控: COM1/COM2 此刻任一被占 (并行席位) → 跳过, 不硬撞不误判。"""
    busy = []
    for name in (BRIDGE_COM, PEER_COM):
        try:
            s = serial.Serial(port=name)
            s.close()
        except serial.SerialException:
            busy.append(name)
    if busy:
        pytest.skip(f"[BLOCKED-BY-ENVIRONMENT] {'/'.join(busy)} 被并行席位占用, "
                    "串口运行态用例让行 (非产品违约)")


# ------------------------------------------------------------------ 清单工具

def _export(api: Bridge) -> dict:
    """GET /api/fleet/export → 原样 fleet 文档 (FR-22; dict 信封, 探针实测含
    version/bridges/manager/window)。"""
    code, body = api.get("/api/fleet/export")
    assert code == 200 and isinstance(body, dict), \
        f"GET /api/fleet/export -> {code}: {str(body)[:200]!r}"
    assert isinstance(body.get("bridges"), list), f"export 缺 bridges 集合: {str(body)[:200]!r}"
    return body


def _import(api: Bridge, mode: str, doc) -> None:
    """POST /api/fleet/import (对象编码, fr22 套件实证主路径), 受理判据 ADR-5③ 家族。"""
    code, body = api.post("/api/fleet/import", {"mode": mode, "json": doc})
    post_accepted(code, body, f"/api/fleet/import ({mode})")


def _entry_of(doc: dict, name: str) -> dict:
    hits = [e for e in doc["bridges"] if e.get("name") == name]
    assert len(hits) == 1, f"清单中桥 {name!r} 应恰 1 条, 实得 {len(hits)}"
    return copy.deepcopy(hits[0])


def _derived(template: dict, name: str, bid: str, serial_port: str, listen: int) -> dict:
    """由既有条目派生新桥条目: 只改 name/id/串口/listen, 其余字段保持清单形状。"""
    e = copy.deepcopy(template)
    e["name"] = name
    e["id"] = bid
    s = e.get("serial")
    if isinstance(s, dict):
        s["port"] = serial_port
    else:
        e["serial"] = serial_port
    e["listen"] = f"{HTTP_HOST}:{listen}"
    return e


def _read_fleet(fleet_path: Path) -> dict:
    return json.loads(fleet_path.read_text("utf-8"))


def _open_bridge(api: Bridge, name: str, serial_port: str, listen: int) -> str:
    """建桥 (autoOpen) + 启动 + 等 open, 返回桥 id。

    等 open 失败且桥行 lastError 为「拒绝访问」→ 并行席位抢占了串口 (本套件前置
    serial_pair_free 刚验过空闲), 让行为环境 skip —— 不算产品违约。
    """
    code, resp = fleet_create(api, name, serial_port, listen, extra={"autoOpen": True})
    assert code in (200, 201) and not (isinstance(resp, dict) and resp.get("ok") is False), \
        f"POST /api/fleet -> {code}: {resp!r}"
    bid = fleet_new_id(api, resp, listen)
    fleet_act_ok(api, bid, "start")
    try:
        wait_row_phase(api, bid, "open", timeout=15)
    except AssertionError:
        row = row_of(api, bid)
        err = str((row or {}).get("lastError") or "")
        if "拒绝访问" in err:
            pytest.skip(f"[BLOCKED-BY-ENVIRONMENT] 建桥后串口即被并行席位抢占 "
                        f"(lastError={err!r}), 运行态用例让行")
        raise
    return bid


def _watch_serial_released(port_name: str, action, timeout: float = 10.0) -> bool:
    """高频 (50ms) 监视串口释放: **先**起 watcher 再执行 action, 限时内观察到 ≥1 次
    「可打开」即判已释放。

    背景 (探针实测, 2026-09-15): replace 导入后旧桥串口 ≈0.1s 内即释放, 但并行席位
    (output/ui-xtest 实例, autoOpen 桥 3s 重试) 会在释放后立刻抢回 —— 低频轮询
    (conftest wait_serial_free 0.2s) 与限时断言会被「抢回」误判成未释放。
    A 此刻必然持有该口 (open 已断言, Windows 独占), 故 free 采样 ⇒ 释放已发生。
    测量方法修订, 预期本身不变 (沿 qa-sprint13-plan §6.3 R4 同纪律)。
    """
    events: list[str] = []
    stop = threading.Event()

    def _watch():
        while not stop.is_set():
            try:
                s = serial.Serial(port=port_name)
                s.close()
                events.append("free")
            except serial.SerialException:
                events.append("busy")
            time.sleep(0.05)

    th = threading.Thread(target=_watch, daemon=True)
    th.start()
    try:
        action()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if "free" in events:
                return True
            time.sleep(0.05)
        return False
    finally:
        stop.set()
        th.join(timeout=2)


def _wait_open_tolerant(api: Bridge, ident, timeout: float = 15.0) -> tuple[dict | None, str]:
    """等桥 open, 不因环境抢占而中断用例: 返回 (open 时的行 | None, 收尾判据)。

    open 未达成时返回 None; 调用方在用例尾部据此区分: lastError 含「拒绝访问」
    且行健在 → 并行席位抢占 (环境), 否则为产品违约。
    """
    deadline = time.monotonic() + timeout
    row = None
    while time.monotonic() < deadline:
        row = row_of(api, ident)
        if row is not None and row.get("phase") == "open":
            return row, "open"
        time.sleep(0.1)
    err = str((row or {}).get("lastError") or "")
    return None, err


# ==================================================================
# 1  导入 × 运行态 (FR-22 × FR-10g)
# ==================================================================

def test_xt1_import_replace_stops_running_and_merge_restores(
        spawn_fleet, fr22_ready, serial_pair_free):
    """A 运行中 → replace 导入含 B 清单: A 停/替换 (行消失+串口释放), B 按清单启动;
    merge 导入含 A 清单: A 回来且 B 不受影响。

    串口释放用 50ms watcher 判定 (先起监视再导入, 见 _watch_serial_released);
    open 态遇并行席位抢占 (lastError 拒绝访问) 在用例尾部降级为环境 skip,
    其余断言全部先行完成 —— 违约与环境干扰分账。
    """
    api = spawn_fleet(port=take_port(18400))
    fleet_purge(api)
    port_a, port_b = take_port(18401), take_port(18402)

    id_a = _open_bridge(api, "qa-xt-a", BRIDGE_COM, port_a)      # 桥 A 运行中 (open)
    doc = _export(api)
    entry_a = _entry_of(doc, "qa-xt-a")
    entry_b = _derived(entry_a, "qa-xt-b", "qa-xt-b-id", PEER_COM, port_b)

    # --- replace 导入含桥 B 的清单 (不含 A); watcher 证 A 串口已释放 ---
    released = _watch_serial_released(
        BRIDGE_COM, lambda: _import(api, "replace", {"version": 1, "bridges": [entry_b]}))

    assert row_of(api, id_a) is None, \
        f"replace 后旧桥 A 应被替换移除: rows={[r['name'] for r in fleet_rows(api)]!r}"
    assert released, ("replace 后 10s 内未观察到 A 串口释放 (50ms 监视) —— "
                      "「运行中桥先停」未真停 (违约)")

    row_b, b_err = _wait_open_tolerant(api, "qa-xt-b-id", timeout=15)
    assert row_b is not None, "replace 后 B 未在列 (导入未按清单建桥启动)"
    assert serial_port_of(row_b) == PEER_COM and row_b["running"] is True, \
        f"B 未按清单启动: {row_b!r}"
    assert listen_port_of(row_b["listen"]) == port_b, f"B listen 不符: {row_b!r}"
    pre_b_phase = row_b["phase"]                                 # open 或 retry(被抢占)

    # --- merge 导入含桥 A 的清单 ---
    wait_port_free(port_a)                                       # A 数据口异步释放, 等可绑
    _import(api, "merge", {"version": 1, "bridges": [entry_a]})

    row_a = row_of(api, id_a)
    assert row_a is not None, "merge 后 A 未回来"
    assert serial_port_of(row_a) == BRIDGE_COM \
        and listen_port_of(row_a["listen"]) == port_a, f"A 回归但配置不符: {row_a!r}"
    row_a_open, a_err = _wait_open_tolerant(api, id_a, timeout=15)

    # --- B 不受影响 (合并不得扰动既有桥: 身份/配置/运行态) ---
    row_b2 = row_of(api, "qa-xt-b-id")
    assert row_b2 is not None, "merge 后 B 竟消失"
    assert serial_port_of(row_b2) == PEER_COM \
        and listen_port_of(row_b2["listen"]) == port_b \
        and row_b2["running"] is True, f"merge 波及了 B 的配置/运行态: {row_b2!r}"
    if pre_b_phase == "open":
        assert row_b2["phase"] == "open", \
            f"merge 扰动了已 open 的 B: {pre_b_phase} → {row_b2['phase']}"
    else:                                                        # B 曾被抢占 (retry)
        assert row_b2["phase"] in ("open", "retry"), \
            f"B 相态越出预期: {row_b2['phase']!r}"
    assert {r["name"] for r in fleet_rows(api)} == {"qa-xt-a", "qa-xt-b"}, \
        f"终态桥集合不符: {fleet_rows(api)!r}"

    # --- A 回归 open 收尾判定 (放最后, 环境抢占只降级此处) ---
    if row_a_open is None:
        if "拒绝访问" in a_err:
            pytest.skip("[BLOCKED-BY-ENVIRONMENT] A 回归后串口被并行席位抢占, open 态未观测; "
                        "A 已按清单恢复 (行/配置/运行态断言均通过)")
        raise AssertionError(
            f"merge 导入未按清单启动 A (违约, lastError={a_err!r})")


# ==================================================================
# 2  导入 × 窗口记忆 (FR-22 × ADR-22①, 真实风险点)
# ==================================================================

def test_xt2_import_preserves_window_section(spawn_fleet, fr22_ready):
    """种子 [window] 段 → replace/merge 导入 (清单不含 window 键) → 盘上 window 段
    必须原样保留; 导入的 bridges 变更须真实生效 (防假阳)。"""
    fleet_file = Path(tempfile.mkdtemp(prefix="serialhub_xtest_win_")) / "fleet.json"
    fleet_file.write_text(json.dumps({"version": 1, "window": WINDOW_SEED, "bridges": []}),
                          encoding="utf-8")
    api = spawn_fleet(port=take_port(18410), fleet_path=fleet_file)
    fleet_purge(api)
    assert _read_fleet(fleet_file).get("window") == WINDOW_SEED, \
        "前置失效: 启动+清场后 window 段已不在 (BUG-1 回归, 未到导入步骤)"

    code, resp = fleet_create(api, "qa-xt-c", "COM99", take_port(18411))
    assert code in (200, 201), f"建桥失败: {code}: {resp!r}"
    # create 响应必带 id (ADR-14③ 受理形状; qa-sprint13 黑盒实证)
    id_c = resp.get("id") if isinstance(resp, dict) else None
    assert id_c is not None, f"create 响应无 id: {resp!r}"
    assert _read_fleet(fleet_file).get("window") == WINDOW_SEED, \
        "建桥 persist 已抹掉 window (未到导入步骤即违约)"

    doc = _export(api)
    entry_c = _entry_of(doc, "qa-xt-c")
    entry_d = _derived(entry_c, "qa-xt-d", "qa-xt-d-id", "COM99", take_port(18412))

    # --- replace 导入 (清单无 window 键) ---
    _import(api, "replace", {"version": 1, "bridges": [entry_d]})
    data = _read_fleet(fleet_file)
    assert data.get("window") == WINDOW_SEED, \
        f"replace 导入抹掉/改写了 [window] 段 (ADR-22① × FR-22 违约): {data.get('window')!r}"
    assert [b.get("id") for b in data.get("bridges", [])] == ["qa-xt-d-id"], \
        f"导入未生效 (防假阳检查失败): {data.get('bridges')!r}"

    # --- merge 导回 C, window 仍须保留 ---
    _import(api, "merge", {"version": 1, "bridges": [entry_c]})
    data = _read_fleet(fleet_file)
    assert data.get("window") == WINDOW_SEED, \
        f"merge 导入抹掉/改写了 [window] 段: {data.get('window')!r}"
    assert {b.get("id") for b in data.get("bridges", [])} == {"qa-xt-d-id", id_c}, \
        f"merge 后桥集合不符: {data.get('bridges')!r}"


# ==================================================================
# 3  导入 × 录像文件 (FR-22 × FR-19「桥删除录像保留」)
# ==================================================================

def _pair_free() -> bool:
    """COM1/COM2 当前是否空闲 (探测性开合, 不占口)。"""
    for name in (BRIDGE_COM, PEER_COM):
        try:
            s = serial.Serial(port=name)
            s.close()
        except serial.SerialException:
            return False
    return True


def test_xt3_import_replace_keeps_old_bridge_recordings(
        spawn_fleet, fr19_ready, fr22_ready):
    """A 录像落盘 → replace 导入不含 A 的清单 (A 被删除) → recordings/ 文件逐字节保留。

    双路径 (环境自适应, 契约同一条): COM1 空闲 → 真 open 桥 + WS TX 载荷 (强证据,
    录像非空); 被并行席位占 → COM99 retry 桥 (record/start 受理, 文件仍由本桥录制
    产出, 探针实测串口 closed 时 TX 不落盘 → 文件可为空, 存活性断言不变)。
    """
    api = spawn_fleet(port=take_port(18420))
    fleet_purge(api)
    listen = take_port(18421)
    strong = _pair_free()
    if strong:
        id_a = _open_bridge(api, "qa-xt-rec", BRIDGE_COM, listen)
    else:
        code, resp = fleet_create(api, "qa-xt-rec", "COM99", listen,
                                  extra={"autoOpen": True})
        assert code in (200, 201) and not (isinstance(resp, dict)
                                           and resp.get("ok") is False), \
            f"POST /api/fleet -> {code}: {resp!r}"
        id_a = fleet_new_id(api, resp, listen)

    before = list_recordings()
    code, body = api.post(f"/api/fleet/{id_a}/record/start", {})
    post_accepted(code, body, "record/start")
    th, done, box = ws_push(f"ws://{HTTP_HOST}:{listen}/ws",
                            [b"QA-XT3-TX-PAYLOAD-0123456789"])
    assert done.wait(10), "WS TX 发射超时"
    if box.get("error"):
        raise box["error"]
    time.sleep(0.3)
    code, body = api.post(f"/api/fleet/{id_a}/record/stop", {})
    post_accepted(code, body, "record/stop")
    time.sleep(0.4)                                          # 落盘窗 (fr19 同款)
    new_files = sorted(list_recordings() - before)
    assert len(new_files) >= 1, f"录制未产出录像文件 (前置失效): {list_recordings()!r}"
    snapshot = {n: (recordings_dir() / n).read_bytes() for n in new_files}
    if strong:
        assert all(len(b) > 0 for b in snapshot.values()), "录像文件为空 (强路径前置失效)"

    doc = _export(api)
    entry_a = _entry_of(doc, "qa-xt-rec")
    entry_d = _derived(entry_a, "qa-xt-d", "qa-xt-d-id", "COM99", take_port(18422))
    try:
        _import(api, "replace", {"version": 1, "bridges": [entry_d]})

        assert row_of(api, id_a) is None, "replace 后旧桥 A 应被移除 (导入须真实生效)"
        for n, data in snapshot.items():
            p = recordings_dir() / n
            assert p.exists(), f"FR-19 违约 (经导入删除路径): 旧桥录像 {n} 被清掉"
            assert p.read_bytes() == data, f"旧桥录像 {n} 内容被改写"
    finally:
        delete_recordings(new_files)                          # 测试卫生: 自产自清


# ==================================================================
# 4  单实例 × fleet (FR-16 × FR-10/ADR-22①)
# ==================================================================

def test_xt4_second_instance_leaves_fleet_untouched(spawn_fleet, fleet_ready):
    """A (有桥+window 段) 运行中 → 第二实例 headless 同管理台端口+同清单 → 友好退出
    (exit 0), A 存活, fleet.json 逐字节不变 (window/bridges 段不被写坏)。"""
    fleet_file = Path(tempfile.mkdtemp(prefix="serialhub_xtest_si_")) / "fleet.json"
    listen = take_port(18430)
    entry = {"id": "b1", "name": "qa-xt-si", "autoOpen": True, "autoReconnect": True,
             "maxClients": 16, "listen": f"{HTTP_HOST}:{listen}",
             "serial": {"port": BRIDGE_COM, "baud": 115200, "dataBits": 8,
                        "parity": "N", "stopBits": 1, "flow": "none"}}
    fleet_file.write_text(
        json.dumps({"version": 1, "window": WINDOW_SEED, "bridges": [entry]}),
        encoding="utf-8")
    api = spawn_fleet(port=take_port(18431), fleet_path=fleet_file)

    # A 恢复出桥 (phase open/retry 皆可 —— COM1 可能被并行席位占; FR-16 探测只依赖 /api/status)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline and row_of(api, "b1") is None:
        time.sleep(0.1)
    assert row_of(api, "b1") is not None, f"A 未恢复出清单中的桥: {fleet_rows(api)!r}"
    time.sleep(1.0)                                          # 恢复期写盘落定
    before = fleet_file.read_bytes()

    # 第二实例: headless, 同管理台端口, 同清单 (写坏风险最大化场景)
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    try:
        proc = subprocess.run(
            [str(BRIDGE_EXE), "--headless", "--addr", api.base.split("//", 1)[1],
             "--fleet", str(fleet_file)],
            capture_output=True, timeout=30, creationflags=flags)
        rc = proc.returncode
        err = proc.stderr.decode("utf-8", "replace")
    except subprocess.TimeoutExpired as e:
        raise AssertionError(f"第二实例 30s 未退出 (FR-16 应快速友好退出): {e!r}") from None
    assert rc == 0, (
        f"FR-16 违约 (fleet 形态交叉): 第二实例应友好退出 (0), 实得 {rc}; "
        f"stderr={err[-200:]!r}")

    assert api.proc.poll() is None, "第二实例退出后 A 竟退出 (不得误伤)"
    assert http_get(api.base + "/api/fleet")[0] == 200, "第二实例退出后 A 的控制面不再服务"
    after = fleet_file.read_bytes()
    assert after == before, (
        "第二实例写坏了 A 的 fleet.json (逐字节对比):\n"
        f"before={before[:400]!r}\nafter={after[:400]!r}")
    data = json.loads(after)
    assert data.get("window") == WINDOW_SEED, f"window 段被改写: {data.get('window')!r}"
    assert [b.get("id") for b in data.get("bridges", [])] == ["b1"], \
        f"bridges 段被改写: {data.get('bridges')!r}"


# ==================================================================
# 5  主题持久化 × 重启 (FR-14, 真浏览器侧)
# ==================================================================

def _npm_global_root() -> str | None:
    for cmd in (["npm.cmd", "root", "-g"], ["npm", "root", "-g"]):
        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
            if r.returncode == 0 and (r.stdout or "").strip():
                return (r.stdout or "").strip().splitlines()[-1]
        except Exception:
            continue
    appdata = os.environ.get("APPDATA")
    if appdata and (Path(appdata) / "npm" / "node_modules").is_dir():
        return str(Path(appdata) / "npm" / "node_modules")
    return None


def _run_pw(mode: str, url: str, profile: Path) -> dict:
    env = dict(os.environ)
    node_root = _npm_global_root()
    if node_root:
        env["NODE_PATH"] = node_root
    try:
        r = subprocess.run(["node", str(PW_SCRIPT), mode, url, str(profile)],
                           capture_output=True, text=True, timeout=PW_TIMEOUT_S, env=env)
    except FileNotFoundError:
        pytest.skip("[BLOCKED-BY-TOOLING] node 不可用 (主题记忆用例需 Playwright)")
    lines = [ln for ln in (r.stdout or "").strip().splitlines() if ln.strip()]
    assert lines, f"pw_theme_persist.cjs 无输出 (rc={r.returncode}) stderr={r.stderr[-300:]!r}"
    try:
        payload = json.loads(lines[-1])
    except json.JSONDecodeError:
        raise AssertionError(f"探针输出非 JSON: {lines[-1]!r}") from None
    assert payload.get("ok") is True, f"主题探针 {mode} 失败: {payload}"
    return payload


def _raw_status(url: str) -> int:
    """原始 GET 状态码 (不 JSON 解析 —— /themes/*.css 是 CSS 文本, http_get 会炸)。"""
    import http.client
    host, _, port = url.split("//", 1)[1].partition(":")
    conn = http.client.HTTPConnection(host, int(port), timeout=5)
    try:
        conn.request("GET", "/themes/win95.css")
        return conn.getresponse().status
    finally:
        conn.close()


def test_xt5_theme_memory_survives_browser_restart(spawn_fleet, fleet_ready):
    """UI 切 win95 → 浏览器重启 (同 profile 新进程) → 记忆生效 (FR-14)。
    壳 webview 侧留人工项 (qa-sprint11 §3 先例)。"""
    api = spawn_fleet(port=take_port(18440))
    code = _raw_status(api.base)
    assert code == 200, f"前置失效: /themes/win95.css -> {code} (win95 内置主题应可服务)"

    profile = Path(tempfile.mkdtemp(prefix="sh_xt_theme_profile_"))
    _run_pw("set", api.base, profile)          # 走 UI 设置切 win95 (不直写 localStorage)
    payload = _run_pw("verify", api.base, profile)   # 全新浏览器进程 = 重启
    assert payload.get("stored") == "win95", \
        f"重启后 localStorage 记忆丢失: {payload!r}"
    assert payload.get("applied") == "win95.css", \
        f"重启后主题未按记忆应用: {payload!r}"
