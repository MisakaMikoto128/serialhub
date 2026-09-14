# -*- coding: utf-8 -*-
"""FR-10 桥接管理器黑盒套件 (主文件) — ADR-13 / spec FR-10。

覆盖 (预期只来自 spec FR-10g + 任务契约, 不读 src/ 调预期):
1  FR-10a/b 多桥并存: 2 座桥 (COM1/COM2 虚拟对各占一端) 同时 open, 跨对回环逐字节, 计数对账互不串扰;
2  FR-10b/g fleet CRUD: 建桥/列表 21 字段 (契约随动 15→17→19→21, Sprint 13 ADR-24, 见 conftest FLEET_ROW_FIELDS 注)/详情/start→open/stop→closed/delete→移除且端口释放;
3  FR-10b 端口冲突: 同 listen 建第二桥被拒 (400 含"占用"或 ok:false), 原桥不受损;
5  FR-10f 端点稳定: 桥 listen 全生命周期不变; 串口对端断开重接数据面自愈; 缺席串口 retry 可见 (FR-3);
6  FR-10g tap: /api/fleet/<id>/tap 旁看串口 RX, 与数据面 ws 并存互不干扰。

后端未实现时经 fleet_ready 门控整组跳过 [BLOCKED-BY-BACKEND] (见 conftest)。
拓扑注: 只有一对虚拟串口, 双桥同时 open 的唯一可行回环 = 8101 ⇄ COM1 ⇄ COM2 ⇄ 8102 链路
(每桥各占一端, 以对端桥为回环对端), 详见 docs/team/reports/qa-sprint4-plan.md。

运行: 在项目根执行  python -m pytest tests/test_fr10_fleet.py -v
"""
from __future__ import annotations

import json
import random
import time

from conftest import (
    BRIDGE_COM,
    HTTP_HOST,
    PEER_COM,
    assert_row_shape,
    fleet_act_ok,
    fleet_create,
    fleet_new_id,
    fleet_purge,
    fleet_rows,
    listen_port_of,
    read_exactly,
    row_of,
    serial_port_of,
    serial_write_all,
    take_port,
    wait_counters,
    wait_port_free,
    wait_row_phase,
    ws_collect_exact,
    ws_push,
)


def _data_url(listen_port: int) -> str:
    return f"ws://{HTTP_HOST}:{listen_port}/ws"


def _start_open_bridge(api, name: str, serial_port: str, listen_port: int,
                       timeout: float = 15.0):
    """建桥 + 启动 + 等 open, 返回 (id, 首次读到的行)。"""
    code, resp = fleet_create(api, name, serial_port, listen_port)
    _post_create_ok(code, resp)
    bid = fleet_new_id(api, resp, listen_port)
    fleet_act_ok(api, bid, "start")
    row = wait_row_phase(api, bid, "open", timeout=timeout)
    return bid, row


def _post_create_ok(code: int, resp) -> None:
    assert code in (200, 201) and not (isinstance(resp, dict) and resp.get("ok") is False), \
        f"POST /api/fleet -> {code}: {resp!r}"


# ------------------------------------------------------------------ 1 多桥并存

def test_fr10a_two_bridges_coexist_and_isolate(start_fleet, fleet_ready):
    api = start_fleet()
    fleet_purge(api)                     # 清掉播种的空白兼容桥, 行数断言才确定
    port_a = take_port(8101)   # 桥 A: serial COM1 (任务示例端口, 被占则自动退临时端口)
    port_b = take_port(8102)   # 桥 B: serial COM2
    id_a, row_a = _start_open_bridge(api, "qa-a", BRIDGE_COM, port_a)
    id_b, row_b = _start_open_bridge(api, "qa-b", PEER_COM, port_b)
    assert id_a != id_b, f"两桥 id 撞车: {id_a!r}"
    rows = fleet_rows(api)
    assert len(rows) == 2, f"应恰两座桥并存: {rows!r}"
    for r in (row_a, row_b):
        assert_row_shape(r)
    assert listen_port_of(row_a["listen"]) == port_a
    assert listen_port_of(row_b["listen"]) == port_b

    # 跨对回环: p1 入 8101 → COM1 → COM2 → 桥B → 8102; p2 反向。逐字节 + 静默守窗。
    p1 = random.Random(0x10A).randbytes(1024)
    p2 = random.Random(0x10B).randbytes(1024)
    col_a, ready_a, box_a = ws_collect_exact(_data_url(port_a), p2, quiet_s=1.5)
    col_b, ready_b, box_b = ws_collect_exact(_data_url(port_b), p1, quiet_s=1.5)
    assert ready_a.wait(10) and ready_b.wait(10), "数据面 WS 未就绪"
    push_a, done_a, pbox_a = ws_push(_data_url(port_a), [p1])
    push_b, done_b, pbox_b = ws_push(_data_url(port_b), [p2])
    for th, done, box in ((push_a, done_a, pbox_a), (push_b, done_b, pbox_b)):
        th.join(15)
        assert done.wait(5), "发射器未在限时内完成"
        if box.get("error"):
            raise box["error"]
    col_a.join(45)
    col_b.join(45)
    if box_a.get("error"):
        raise box_a["error"]
    if box_b.get("error"):
        raise box_b["error"]
    assert box_a["payload"] == p2, "A 桥数据面收到的下行流不等于 B 桥上行流 (丢/改字节)"
    assert box_b["payload"] == p1, "B 桥数据面收到的下行流不等于 A 桥上行流 (丢/改字节)"

    # 逐桥计数器对账: A.tx=p1 A.rx=p2 B.tx=p2 B.rx=p1 —— 串扰/计数混账在此现形
    wait_counters(api, port_a, want_tx=len(p1), want_rx=len(p2))
    wait_counters(api, port_b, want_tx=len(p2), want_rx=len(p1))

    # 客户端全断开后 clients 归零 (无泄漏)
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        ra, rb = row_of(api, port_a), row_of(api, port_b)
        if ra and rb and ra["clients"] == 0 and rb["clients"] == 0:
            break
        time.sleep(0.1)
    assert ra["clients"] == 0 and rb["clients"] == 0, \
        f"客户端断开后 clients 未归零: A={ra!r} B={rb!r}"


# ------------------------------------------------------------------ 2 fleet CRUD

def test_fr10g_fleet_crud_and_list_contract(start_fleet, fleet_ready):
    api = start_fleet()
    fleet_purge(api)
    port = take_port()
    id_b, _ = _start_open_bridge(api, "qa-crud", BRIDGE_COM, port)

    rows = fleet_rows(api)
    assert len(rows) == 1, f"应恰一座桥: {rows!r}"
    row = rows[0]
    assert row["id"] == id_b, f"列表 id 与建桥响应不一致: {row!r}"
    assert_row_shape(row)                                  # 21 字段齐全 (Sprint 13 契约随动, 见 conftest)
    assert row["name"] == "qa-crud", f"name 未回显: {row!r}"
    assert serial_port_of(row) == "COM1", f"serial 未回显: {row!r}"
    assert listen_port_of(row["listen"]) == port, f"listen 未回显: {row!r}"

    code, detail = api.get(f"/api/fleet/{id_b}")           # FR-10g 详情端点
    assert code == 200 and isinstance(detail, dict), \
        f"GET /api/fleet/{id_b} -> {code}: {detail!r}"

    fleet_act_ok(api, id_b, "stop")
    wait_row_phase(api, id_b, "closed", timeout=10)        # stop → closed

    fleet_act_ok(api, id_b, "delete")
    rows = fleet_rows(api)
    assert all(r.get("id") != id_b for r in rows), f"delete 后列表仍含该桥: {rows!r}"
    code, detail = api.get(f"/api/fleet/{id_b}")
    assert code >= 400, f"delete 后详情端点仍可达: {code}: {detail!r}"
    wait_port_free(port)                                   # 端口释放判据


# ------------------------------------------------------------------ 3 端口冲突

def test_fr10_create_rejects_occupied_listen(start_fleet, fleet_ready):
    api = start_fleet()
    fleet_purge(api)
    port = take_port()
    id_1, _ = _start_open_bridge(api, "qa-occupy", BRIDGE_COM, port)  # open ⇒ listen 已绑定

    code, resp = fleet_create(api, "qa-clash", PEER_COM, port)        # 第二桥同 listen
    clash = (code == 400 and "占用" in json.dumps(resp, ensure_ascii=False)) or \
            (isinstance(resp, dict) and resp.get("ok") is False)
    assert clash, f"同 listen 建第二桥未被拒: code={code} body={resp!r}"

    rows = fleet_rows(api)
    assert len(rows) == 1 and rows[0]["id"] == id_1, \
        f"被拒建桥不得污染列表: {rows!r}"
    assert rows[0]["phase"] == "open", f"原桥受冲突波及: {rows[0]!r}"


# ------------------------------------------------------------------ 5 端点稳定 + 对端重接

def test_fr10f_listen_stable_and_peer_reconnect(start_fleet, fleet_ready, make_peer):
    api = start_fleet()
    port = take_port()
    id_1, row = _start_open_bridge(api, "qa-stable", BRIDGE_COM, port)
    seen = [listen_port_of(row["listen"])]

    peer = make_peer()                                     # 对端就绪
    up1 = random.Random(0x5A1).randbytes(384)
    push, done, pbox = ws_push(_data_url(port), [up1])
    got = read_exactly(peer, len(up1), deadline_s=20)
    push.join(10)
    assert done.wait(5) and not pbox.get("error"), f"上行发射器异常: {pbox}"
    assert got == up1, "断开前上行数据不一致"
    seen.append(listen_port_of(row_of(api, port)["listen"]))

    peer.close()                                           # 对端断开
    time.sleep(1.0)
    assert row_of(api, port)["phase"] == "open", \
        f"对端断开竟扰动桥相位: {row_of(api, port)!r}"

    peer2 = make_peer()                                    # 对端重接 → 数据面自愈 (无需任何干预)
    up2 = random.Random(0x5A2).randbytes(384)
    push2, done2, pbox2 = ws_push(_data_url(port), [up2])
    got2 = read_exactly(peer2, len(up2), deadline_s=20)
    push2.join(10)
    assert done2.wait(5) and not pbox2.get("error"), f"重接后发射器异常: {pbox2}"
    assert got2 == up2, "对端重接后上行数据不一致 (FR-3 自愈违约)"
    peer2.close()
    seen.append(listen_port_of(row_of(api, port)["listen"]))

    fleet_act_ok(api, id_1, "stop")
    wait_row_phase(api, id_1, "closed", timeout=10)
    seen.append(listen_port_of(row_of(api, port)["listen"]))
    fleet_act_ok(api, id_1, "start")
    wait_row_phase(api, id_1, "open", timeout=15)
    seen.append(listen_port_of(row_of(api, port)["listen"]))
    assert seen == [port] * len(seen), f"FR-10f 端点稳定违约, 各阶段 listen={seen}"

    # 缺席串口的桥: FR-3 监督状态机在每桥行可见 (retry + lastError)
    ghost_port = take_port()
    code, resp = fleet_create(api, "qa-ghost", "COM99", ghost_port)
    _post_create_ok(code, resp)
    gid = fleet_new_id(api, resp, ghost_port)
    fleet_act_ok(api, gid, "start")
    grow = wait_row_phase(api, gid, "retry", timeout=20)
    assert grow["lastError"], f"retry 桥 lastError 为空: {grow!r}"
    fleet_act_ok(api, gid, "delete")                       # retry 态可直接删除 (契约未设前置)
    assert row_of(api, gid) is None, "ghost 桥删除失败"


# ------------------------------------------------------------------ 6 tap 旁看

def test_fr10g_tap_receives_serial_rx_alongside_data_plane(start_fleet, fleet_ready,
                                                           make_peer):
    api = start_fleet()
    port = take_port()
    id_1, _ = _start_open_bridge(api, "qa-tap", BRIDGE_COM, port)
    peer = make_peer()

    payload = random.Random(0x7A9).randbytes(512)
    tap_th, tap_ready, tap_box = ws_collect_exact(
        f"ws://{HTTP_HOST}:{api.http_port}/api/fleet/{id_1}/tap", payload,
        allow_text=True)                                   # tap 旁看 (任务契约只锁 RX 字节)
    dp_th, dp_ready, dp_box = ws_collect_exact(_data_url(port), payload)  # 数据面客户端
    assert tap_ready.wait(10) and dp_ready.wait(10), "tap/数据面 WS 未就绪"

    serial_write_all(peer, payload)                        # 串口 RX → 双路同见
    tap_th.join(30)
    dp_th.join(30)
    if tap_box.get("error"):
        raise tap_box["error"]
    if dp_box.get("error"):
        raise dp_box["error"]
    assert tap_box["payload"] == payload, "tap 未完整收到串口 RX 字节"
    assert dp_box["payload"] == payload, "数据面客户端未完整收到串口 RX 字节"

    down = random.Random(0x7AA).randbytes(256)             # tap 并存不干扰数据面 TX
    push, done, pbox = ws_push(_data_url(port), [down])
    got = read_exactly(peer, len(down), deadline_s=15)
    push.join(10)
    assert done.wait(5) and not pbox.get("error"), f"发射器异常: {pbox}"
    assert got == down, "tap 挂接期间数据面 TX 受扰"
