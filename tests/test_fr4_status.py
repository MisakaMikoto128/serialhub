# -*- coding: utf-8 -*-
"""FR-4 状态接口 + ADR-5 契约裁定。

ADR-5: ① status 恰 9 字段 phase/port/baud/config/clients/rxBytes/txBytes/lastError/
uptimeSec, 不含 flow; ② uptimeSec = 进程运行时长; ③ POST 成功 {"ok":true},
失败 400+{"ok":false,"error"}; ④ /api/open 在已打开态为 no-op。
"""
from __future__ import annotations

import threading
import time

from conftest import (join_collectors, read_exactly,
                      serial_write_all, start_ws_collectors, status_fields_expected,
                      wait_all_ready, ws_send_and_hold)


def test_fr4_status_contract(start_bridge, make_peer):
    """/api/status 恰 13 字段 (ADR-11 修订 ADR-9 ①; ADR-15① Sprint 5 随动修订 11→12 增 retries;
    ADR-16① Sprint 6 随动修订 12→13 增 autoReconnect),
    初始计数与相位合理, uptimeSec 随进程时间增长 (②)。"""
    b = start_bridge()
    st = b.status()
    # ADR-16① 渐进放行: autoReconnect 未落地时按旧 12 字段契约守护, 落地即 13 字段全量
    # (仅容忍该一字段缺席, 其它多/少字段当场违约; 见 conftest.status_fields_expected)
    assert set(st.keys()) == status_fields_expected(st), (
        f"ADR-11/ADR-15①/ADR-16① 字段集合不符: 多 {set(st) - status_fields_expected(st)}, "
        f"少 {status_fields_expected(st) - set(st)}")
    assert isinstance(st["phase"], str)
    assert isinstance(st["port"], str) and st["port"] == "COM1"
    assert st["baud"] == 115200
    assert st["config"] == "8N2"
    assert st["clients"] == 0
    assert st["maxClients"] == 0, \
        f"maxClients 默认应为 0 (不限, ADR-9 ①): 实得 {st['maxClients']!r}"
    assert st["rxBytes"] == 0 and st["txBytes"] == 0, \
        f"初始计数应为 0: rxBytes={st['rxBytes']}, txBytes={st['txBytes']}"
    assert st["retries"] == 0, \
        f"初始 retries 应为 0 (ADR-15①): 实得 {st['retries']!r}"
    assert st["lastError"] in (None, ""), f"无错时 lastError={st['lastError']!r}"
    assert isinstance(st["uptimeSec"], (int, float)) and st["uptimeSec"] >= 0

    b.wait_phase("open")

    # ADR-5 ② uptimeSec = 进程运行时长: close 之后 (串口已关) 仍应继续增长
    u0 = b.status()["uptimeSec"]
    time.sleep(2.2)
    u1 = b.status()["uptimeSec"]
    assert 1 <= (u1 - u0) <= 4, f"uptimeSec 未随进程时间增长: {u0} -> {u1}"
    code, body = b.post("/api/close", {})
    assert code == 200 and body == {"ok": True}
    b.wait_phase("closed")
    u2 = b.status()["uptimeSec"]
    time.sleep(1.5)
    u3 = b.status()["uptimeSec"]
    assert u3 > u2, (
        f"close 后 uptimeSec 停止增长 ({u2} -> {u3}): 疑为'本次打开时长'而非进程运行时长"
        " (违反 ADR-5 ②)")


def test_fr4_status_clients_counter(start_bridge):
    """clients 随 WS 连接/断开增减 (spec FR-4 客户端数)。"""
    b = start_bridge()
    b.wait_phase("open")
    assert b.status()["clients"] == 0

    hold1, hold2 = threading.Event(), threading.Event()
    t1, r1, box1 = ws_send_and_hold(b.ws_url, [], hold1)
    assert r1.wait(10), "WS 客户端 1 连接超时"
    _ = poll_clients(b, 1)
    t2, r2, box2 = ws_send_and_hold(b.ws_url, [], hold2)
    assert r2.wait(10), "WS 客户端 2 连接超时"
    _ = poll_clients(b, 2)

    hold1.set()
    t1.join(10)
    _ = poll_clients(b, 1, desc="客户端 1 断开后 clients 回落到 1")
    hold2.set()
    t2.join(10)
    _ = poll_clients(b, 0, desc="全部断开后 clients 回落到 0")


def poll_clients(b, expected: int, timeout: float = 5.0, desc: str = ""):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = b.status()["clients"]
        if last == expected:
            return last
        time.sleep(0.1)
    raise AssertionError(
        f"{desc or 'clients 计数'}: {timeout}s 内未到 {expected}, 实际 {last}")


def test_fr4_status_counters_grow(start_bridge, make_peer):
    """rxBytes/txBytes 随流量增长且记账精确 (上行 1000B / 下行 1000B 各计入恰一个计数器)。"""
    b = start_bridge()
    b.wait_phase("open")
    ser = make_peer(stopbits=2)
    payload = (bytes(range(256)) * 3 + bytes(range(232)))[:1000]   # 恰 1000B

    s0 = b.status()
    hold = threading.Event()
    th, ready, box = ws_send_and_hold(b.ws_url, [payload], hold)
    assert ready.wait(10)
    read_exactly(ser, 1000, 30)
    hold.set()
    th.join(10)
    if box.get("error"):
        raise box["error"]
    time.sleep(0.4)
    s1 = b.status()

    th2, ready2, box2 = start_ws_collectors(b.ws_url, 1, 1000)
    wait_all_ready(ready2, box2)
    serial_write_all(ser, payload)
    join_collectors(th2, box2, 30)
    time.sleep(0.4)
    s2 = b.status()

    d_up = {k: s1[k] - s0[k] for k in ("rxBytes", "txBytes")}
    d_dn = {k: s2[k] - s1[k] for k in ("rxBytes", "txBytes")}
    assert sorted(d_up.values()) == [0, 1000], \
        f"上行 1000B 后计数增量异常 (期望恰一计数器 +1000): {d_up}"
    assert sorted(d_dn.values()) == [0, 1000], \
        f"下行 1000B 后计数增量异常 (期望恰一计数器 +1000): {d_dn}"
    up_field = next(k for k, v in d_up.items() if v == 1000)
    dn_field = next(k for k, v in d_dn.items() if v == 1000)
    assert up_field != dn_field, \
        f"上行/下行计入同一计数器 {up_field} (rx/tx 方向语义缺失): up={d_up}, dn={d_dn}"


def test_fr4_post_contract_shapes(start_bridge, make_peer):
    """ADR-5 ③④: POST 成功 {"ok":true}; 失败 400+{"ok":false,"error"}; open-while-open = no-op。"""
    b = start_bridge(extra=["--no-open"])     # closed 起步
    b.wait_phase("closed")

    code, body = b.post("/api/config", {"port": "COM1", "baud": 115200,
                                        "dataBits": 8, "parity": "N",
                                        "stopBits": 1, "flow": "none"})
    assert (code, body) == (200, {"ok": True}), \
        f"POST /api/config 成功形状 (ADR-5 ③): {code} {body!r}"
    code, body = b.post("/api/open", {})
    assert (code, body) == (200, {"ok": True}), \
        f"POST /api/open 成功形状: {code} {body!r}"
    b.wait_phase("open")

    # ADR-5 ④ open-while-open = no-op: 返回成功、相位不变、连接不掉、数据面仍通
    code, body = b.post("/api/open", {})
    assert (code, body) == (200, {"ok": True}), \
        f"open-while-open 应 no-op 成功: {code} {body!r}"
    assert b.status()["phase"] == "open"
    ser = make_peer(stopbits=1)
    probe = b"\xA5\x5A" * 8
    hold = threading.Event()
    th, ready, box = ws_send_and_hold(b.ws_url, [probe], hold)
    assert ready.wait(10)
    got = read_exactly(ser, len(probe), 20)
    assert got == probe, "open-while-open 后数据面中断 (no-op 语义被破坏)"
    hold.set()
    th.join(10)
    if box.get("error"):
        raise box["error"]

    code, body = b.post("/api/close", {})
    assert (code, body) == (200, {"ok": True}), f"POST /api/close 成功形状: {code} {body!r}"
    b.wait_phase("closed")

    # 失败形状: 非法 config
    code, body = b.post("/api/config", {"baud": 999999999})
    assert code == 400, f"非法 config 应 400 (ADR-5 ③): {code} {body!r}"
    assert isinstance(body, dict) and body.get("ok") is False and body.get("error"), \
        f"ADR-5 ③ 失败形状不符: {body!r}"


def test_fr4_ports_lists_pair(start_bridge):
    """/api/ports 列本机串口, 必须包含测试对 COM1/COM2 (spec FR-4)。"""
    b = start_bridge()
    code, body = b.get("/api/ports")
    assert code == 200, f"GET /api/ports -> {code}: {body!r}"
    text = repr(body)
    assert "COM1" in text and "COM2" in text, \
        f"/api/ports 未列出测试串口对: {text[:300]}"
