# -*- coding: utf-8 -*-
"""FR-11 串口热拔插自动重连 — QA 演练套件 (ADR-15③④, Sprint 5)。

预期只来自 ADR-15①③④ (docs/team/decisions.md), 不读 src/ 调预期:
- ADR-15① 契约 +`retries` (u32): retry 态每次失败 +1, 成功打开归 0;
  单桥 /api/status 与 /api/fleet 桥对象同步 (11→12 / 13→14 字段)。
- ADR-15③ COM99 缺席 → retries 递增 (黑盒); ELTIMA CLI 调查结论见
  docs/team/reports/qa-sprint5-plan.md —— 无程序化删/建虚拟对手段 → 按任务降级路径覆盖。
- ADR-15④ 客户端零动作: 串口断/恢复期间 WS 客户端连接保持, 恢复后数据自动续传。

覆盖矩阵 (如实的覆盖边界):
1. retries 递增 (fleet, COM99)          —— retries 递增/单调/相位恒 retry;
2. retries 递增/成功归零 (单桥 status)  —— COM99 递增, 换回 COM1 open 成功归 0;
3. retry 态客户端保持 + 零动作自动恢复 —— 真实 retry→open 相变 (占用桥侧串口制造 open 失败,
   释放即恢复), 全程零 API/客户端动作, 数据续传;
4. 串口断/恢复客户端零动作续传 (降级)  —— 控制面 close→open 模拟串口断, 长连 WS 客户端
   ping/pong 存活 + 续传。真实拔插需 ELTIMA CLI/管理员权限 (无), 真机演练指南见
   qa-sprint5-plan.md (COM8 拔插 10 次, 用户执行)。

波2 收口注: 波1 时 #1/#2 曾以 [BLOCKED-BY-BACKEND] 门控, 后端 retries 落地 (契约 12/14
字段) 后已去门控全量放行; 契约字段集随动修订见 conftest STATUS_FIELDS/FLEET_ROW_FIELDS。

注意: 串口只许用 COM1↔COM2 ELTIMA 虚拟对 (方向对称, 本套件沿用 conftest 约定
桥侧=COM1 / 对端=COM2), 全程禁止碰 COM8。COM99 为不存在的幽灵端口。

运行: 在项目根执行  python -m pytest tests/test_fr11_reconnect.py -v
"""
from __future__ import annotations

import asyncio
import random
import threading
import time

import websockets

from conftest import (
    BRIDGE_COM,
    HTTP_HOST,
    fleet_act_ok,
    fleet_create,
    fleet_new_id,
    poll_until,
    row_of,
    serial_write_all,
    take_port,
    wait_row_phase,
)


def _data_url(listen_port: int) -> str:
    return f"ws://{HTTP_HOST}:{listen_port}/ws"


def _create_ok(code: int, resp) -> None:
    assert code in (200, 201) and not (isinstance(resp, dict) and resp.get("ok") is False), \
        f"POST /api/fleet -> {code}: {resp!r}"


def _assert_u32_counter(val, ctx: str) -> None:
    assert isinstance(val, int) and not isinstance(val, bool) and val >= 0, \
        f"{ctx} retries 应为非负整数 (u32, ADR-15①): {val!r}"


def _count_increases(samples: list) -> int:
    return sum(1 for a, b in zip(samples, samples[1:]) if b > a)


def ws_hold_then_collect(ws_url: str, expect: bytes, hold: threading.Event,
                         first: bytes | None = None):
    """零动作长连 WS 客户端: 连接 → [收满 first] → 保持期周期 ping/pong 存活探测
    → hold 释放后收满 expect → 退出。

    客户端结构上无重连路径: 任何断线都以异常进 box["error"], 用例即 FAIL ——
    "客户端零动作/连接保持" 由构造保证并由断言显式验证 (ADR-15④)。
    返回 (thread, ready_event, box); box: error / phase1 / payload / pings / pong_fail。
    """
    ready = threading.Event()
    box: dict = {"pings": 0, "pong_fail": 0}

    def _run():
        async def _main():
            async with websockets.connect(ws_url, max_size=None) as ws:
                ready.set()
                if first is not None:
                    buf = bytearray()
                    while len(buf) < len(first):
                        m = await asyncio.wait_for(ws.recv(), timeout=30)
                        if isinstance(m, (bytes, bytearray)):
                            buf += m
                    box["phase1"] = bytes(buf)
                while not hold.is_set():
                    pong = await ws.ping()          # 存活探测 (ADR-15④)
                    try:
                        await asyncio.wait_for(pong, timeout=5)
                        box["pings"] += 1
                    except asyncio.TimeoutError:
                        box["pong_fail"] += 1
                        raise AssertionError(
                            "保持期 pong 超时 —— 数据面 WS 连接已断 (违反 ADR-15④ 客户端连接保持)")
                    await asyncio.sleep(0.6)
                buf2 = bytearray()
                while len(buf2) < len(expect):
                    m = await asyncio.wait_for(ws.recv(), timeout=45)
                    if isinstance(m, (bytes, bytearray)):
                        buf2 += m
                box["payload"] = bytes(buf2)
        try:
            asyncio.run(_main())
        except BaseException as e:  # noqa: BLE001 — 异常原样带回主线程
            box["error"] = e

    th = threading.Thread(target=_run, daemon=True)
    th.start()
    return th, ready, box


# ==================================================================
# 1) ADR-15③: COM99 缺席 → /api/fleet 桥对象 retries 随时间递增
# ==================================================================

def test_fr11_fleet_retries_increments_on_missing_port(start_fleet, fleet_ready):
    api = start_fleet()
    port = take_port()
    code, resp = fleet_create(api, "qa-fr11-ghost", "COM99", port)
    _create_ok(code, resp)
    bid = fleet_new_id(api, resp, port)
    fleet_act_ok(api, bid, "start")
    row = wait_row_phase(api, bid, "retry", timeout=20)   # FR-3 状态机 (1s 监督重试)
    assert row["lastError"], f"retry 桥 lastError 应非空: {row!r}"
    _assert_u32_counter(row["retries"], "retry 态初始")
    assert row["retries"] >= 1, \
        f"进入 retry 态时首次失败应已计数 (ADR-15①, 黑盒实证 ≥1): {row!r}"

    # 采样 ~7s (0.5s 粒度): 监督任务 1s 间隔 → 应观察到 ≥3 次 retries 严格递增;
    # 递增期间 phase 恒为 retry 且计数不得回退。
    samples, phase_ok = [], True
    t_end = time.monotonic() + 7.0
    while time.monotonic() < t_end:
        row = row_of(api, bid)
        assert row is not None, "采样期间桥行消失"
        phase_ok = phase_ok and row["phase"] == "retry"
        _assert_u32_counter(row["retries"], "采样中")
        samples.append(row["retries"])
        time.sleep(0.5)
    assert phase_ok, f"递增观察窗内 phase 越出 retry: {samples!r}"
    assert _count_increases(samples) >= 3, (
        f"7s 内 retries 严格递增不足 3 次 (监督应为 1s 间隔, ADR-4/ADR-15①): samples={samples}")
    assert all(b >= a for a, b in zip(samples, samples[1:])), \
        f"retries 出现回退 (应为单调不减): {samples}"

    fleet_act_ok(api, bid, "delete")                       # retry 态可直接删除 (FR-10f 同)
    assert row_of(api, bid) is None, "ghost 桥删除失败"


# ==================================================================
# 2) ADR-15①: 单桥 /api/status 同步 retries; 成功打开归 0
# ==================================================================

def test_fr11_status_retries_increments_and_reset_on_open(start_bridge):
    b = start_bridge(port_name="COM99")                    # 缺席端口 → 自动重试循环
    deadline = time.monotonic() + 15
    st = None
    while time.monotonic() < deadline:
        st = b.status()
        if st.get("phase") == "retry" and st.get("lastError"):
            break
        time.sleep(0.1)
    assert st is not None and st.get("phase") == "retry", f"15s 内未进入 retry: {st!r}"
    _assert_u32_counter(st["retries"], "retry 态初始")
    assert st["retries"] >= 1, \
        f"进入 retry 态时首次失败应已计数 (ADR-15①, 黑盒实证 ≥1): {st!r}"

    samples = []
    t_end = time.monotonic() + 7.0
    while time.monotonic() < t_end:
        st = b.status()
        assert st["phase"] == "retry", f"观察窗内 phase 越出 retry: {st!r}"
        _assert_u32_counter(st["retries"], "采样中")
        samples.append(st["retries"])
        time.sleep(0.5)
    assert _count_increases(samples) >= 3, \
        f"7s 内 /api/status retries 递增不足 3 次: samples={samples}"

    # 成功打开归 0 (ADR-15①): 换回 COM1 → open → retries == 0 (FR-3 c 同路)
    code, body = b.post("/api/config", {"port": "COM1", "baud": 115200,
                                        "dataBits": 8, "parity": "N",
                                        "stopBits": 1, "flow": "none"})
    assert code == 200 and body == {"ok": True}, f"POST /api/config -> {code}: {body!r}"
    code, body = b.post("/api/open", {})
    assert code == 200 and body == {"ok": True}, f"POST /api/open -> {code}: {body!r}"
    b.wait_phase("open", timeout=10)
    st = b.status()
    assert st["retries"] == 0, \
        f"成功打开后 retries 应归 0 (ADR-15①), 实得 {st['retries']!r}"


# ==================================================================
# 3) ADR-15④ 关键条: retry 态客户端保持 + 零动作自动恢复 + 数据续传
#    (真实 retry→open 相变: 占住桥侧 COM1 制造 open 失败, 释放即恢复)
# ==================================================================

def test_fr11_retry_phase_client_holds_and_zero_action_recovery(
        start_fleet, fleet_ready, make_peer):
    api = start_fleet()
    blocker = make_peer(port_name=BRIDGE_COM)              # 占住桥侧 COM1 → open 必败
    port = take_port()
    code, resp = fleet_create(api, "qa-fr11-hold", BRIDGE_COM, port)
    _create_ok(code, resp)
    bid = fleet_new_id(api, resp, port)
    fleet_act_ok(api, bid, "start")
    row = wait_row_phase(api, bid, "retry", timeout=20)
    assert row["lastError"], f"占用串口的桥应 retry 且留痕: {row!r}"

    # retry 态下数据面端点可接入 (ADR-13⑤ 端点稳定; 数据面与串口状态解耦)
    payload = random.Random(0x11A).randbytes(768)
    hold = threading.Event()
    th, ready, box = ws_hold_then_collect(_data_url(port), payload, hold)
    assert ready.wait(10), "retry 态下数据面 WS 接入失败 (ADR-13⑤ 端点稳定违约)"

    # 保持期 ≥3s (覆盖 ≥3 个 1s 监督重试周期): 客户端线程周期 ping/pong, 断线即异常;
    # 同步观察 retries 递增; clients 计数 = 该原客户端仍在连接。
    hold_samples = []
    t_end = time.monotonic() + 3.5
    while time.monotonic() < t_end:
        row = row_of(api, port)
        assert row is not None, "retry 保持期桥行消失"
        assert row["clients"] == 1, \
            f"retry 保持期客户端掉线 (ADR-15④ 违约): {row!r}"
        hold_samples.append(row["retries"])
        time.sleep(0.5)
    assert not box.get("error"), f"retry 保持期客户端异常: {box['error']!r}"

    # —— 零动作恢复: 释放 COM1, 无任何 API/客户端动作, 监督任务应自动拉起 ——
    blocker.close()
    t0 = time.monotonic()
    row = wait_row_phase(api, bid, "open", timeout=15)
    recover_s = time.monotonic() - t0
    assert recover_s < 15, f"释放端口后 {recover_s:.1f}s 未自动恢复 open"
    assert row["clients"] == 1, \
        f"恢复瞬间原客户端未保持 (应零动作贯穿): {row!r}"

    # 数据续传: 对端 (pair 另一端 COM2) 灌数, 原客户端 (未重连) 继续收到
    hold.set()
    peer = make_peer()
    serial_write_all(peer, payload)
    th.join(45)
    assert not th.is_alive(), "客户端未在限时内收满续传数据"
    if box.get("error"):
        raise box["error"]
    assert box["payload"] == payload, \
        "恢复后数据续传内容不一致 (丢/改字节, ADR-15④ 违约)"
    assert box["pings"] >= 2 and box["pong_fail"] == 0, \
        f"保持期存活探测不足: pings={box['pings']} pong_fail={box['pong_fail']}"

    # 成功打开归 0 (ADR-15①); 客户端已收满退出, 不再查 clients
    row = row_of(api, port)
    assert row is not None, "恢复后桥行消失"
    _assert_u32_counter(row["retries"], "恢复后")
    assert row["retries"] == 0, \
        f"成功打开后 retries 未归 0 (ADR-15①): {row!r}"
    # retry 保持期 retries 须有累计 (busy→retry 计数从首次失败起)
    assert hold_samples[-1] > 0 or _count_increases(hold_samples) >= 1, \
        f"retry 保持期 retries 未见累计: {hold_samples}"


# ==================================================================
# 4) 降级路径 (任务指定): 控制面 close→open 模拟串口断 — 客户端零动作 + 数据续传
#    真实拔插不可程序化 (ELTIMA 无 CLI, 见 qa-sprint5-plan), 真机演练由用户执行。
# ==================================================================

def test_fr11_serial_cycle_client_zero_action_continuity(start_bridge, make_peer):
    b = start_bridge()                                     # COM1, 自动打开
    b.wait_phase("open")
    peer = make_peer()                                     # 对端 COM2
    first = random.Random(0x11B).randbytes(512)
    second = random.Random(0x11C).randbytes(512)

    hold = threading.Event()
    th, ready, box = ws_hold_then_collect(b.ws_url, second, hold, first=first)
    assert ready.wait(10), "数据面 WS 接入失败"
    serial_write_all(peer, first)                          # 断开前正常传输
    poll_until(lambda: "phase1" in box, timeout=15, desc="客户端收满断开前数据")

    # —— 制造串口断 (降级模拟): close → phase=closed; 数据面 WS 必须保持 ——
    code, body = b.post("/api/close", {})
    assert code == 200 and body == {"ok": True}, f"POST /api/close -> {code}: {body!r}"
    b.wait_phase("closed", timeout=5)
    assert b.status()["clients"] == 1, \
        f"串口断开 (closed) 期间数据面 WS 被断开 (ADR-15④ 违约): {b.status()!r}"
    time.sleep(2.5)                                        # 断开窗: 客户端持续 ping/pong 探测
    if box.get("error"):
        raise box["error"]

    # —— 恢复: open → 数据续传, 客户端全程零动作 ——
    code, body = b.post("/api/open", {})
    assert code == 200 and body == {"ok": True}, f"POST /api/open -> {code}: {body!r}"
    b.wait_phase("open", timeout=10)
    assert b.status()["clients"] == 1, "恢复后原客户端未保持 (发生重连/掉线)"
    serial_write_all(peer, second)                         # 对端再灌 → 客户端续收
    hold.set()
    th.join(45)
    assert not th.is_alive(), "客户端未在限时内收满续传数据"
    if box.get("error"):
        raise box["error"]
    assert box["phase1"] == first, "断开前数据不一致"
    assert box["payload"] == second, "恢复后数据续传内容不一致 (ADR-15④ 违约)"
    assert box["pings"] >= 2 and box["pong_fail"] == 0, \
        f"断开窗存活探测不足: pings={box['pings']} pong_fail={box['pong_fail']}"
