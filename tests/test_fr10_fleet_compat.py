# -*- coding: utf-8 -*-
"""FR-10h CLI 兼容 — 旧单桥参数 = 自动建一座兼容桥, 旧端点全部原样保留。

契约 (spec FR-10h + 任务契约 + ADR-11):
- 旧 CLI (--port COM1 --baud --config --addr) 起进程 → GET /api/fleet 恰见一座桥,
  serial/phase 回显正确, 数据面 listen = 旧地址 (旧 /ws 必须同端口可用);
- 旧 GET /api/status 仍工作且契约不变 (恰 13 字段, ADR-11 + ADR-15①/ADR-16① 随动修订 2026-09-13);
- 旧数据面 /ws 双向回环不回归 (FR-1 语义抽样)。

现有 29 条测试零改动即应全绿 —— 本文件只做"FR-10 落地后旧面不破"的断面复核。
后端未实现时经 fleet_ready 门控跳过 [BLOCKED-BY-BACKEND] (见 conftest)。
"""
from __future__ import annotations

import random
import tempfile
import threading

from conftest import (
    status_fields_expected,
    join_collectors,
    make_peer,
    listen_port_of,
    read_exactly,
    serial_write_all,
    start_ws_collectors,
    wait_all_ready,
    ws_collect_exact,
    ws_push,
    ws_send_and_hold,
    assert_row_shape,
    fleet_rows,
    serial_port_of,
)

from test_fr10_fleet import _data_url


def test_fr10h_legacy_single_bridge_maps_to_one_fleet_row(start_bridge, fleet_ready,
                                                          make_peer):
    b = start_bridge(cwd=tempfile.mkdtemp(prefix="serialhub_fr10_compat_"))  # 旧 CLI 单桥
    b.wait_phase("open", timeout=15)

    # 旧端点: /api/status 契约不变 (恰 13 字段, ADR-11 + ADR-15①/ADR-16① 随动修订;
    # ADR-16① 渐进放行: autoReconnect 未落地按 12 字段守护, 落地即 13 全量)
    code, st = b.get("/api/status")
    assert code == 200, f"旧 GET /api/status -> {code}"
    assert set(st) == status_fields_expected(st), f"旧 status 契约被 FR-10 破坏: {sorted(st)}"
    assert st["retries"] == 0, f"open 兼容桥 retries 应为 0 (ADR-15①): {st!r}"

    # 新控制面视角: 旧单桥参数 ⇒ 恰一座兼容桥
    rows = fleet_rows(b)
    assert len(rows) == 1, f"旧单桥参数应恰映射一座桥: {rows!r}"
    row = rows[0]
    assert_row_shape(row)
    assert serial_port_of(row) == "COM1", f"兼容桥 serial 回显错: {row!r}"
    assert row["phase"] == "open", f"兼容桥应自动启动至 open: {row!r}"
    # FR-10a: 每座桥独立数据端口 (黑盒实证: 兼容桥数据面 = 管理台端口 +1 起探测),
    # 不再与旧地址同端口 —— "旧端点兼容"由 /api/status 200 (上方) 与旧 /ws 同端口
    # 回环 (下方双向) 承载; 兼容桥自身 fleet 数据面 ws://<listen>/ws 亦须可用 (测试末)。
    row_port = listen_port_of(row["listen"])
    assert row_port > 0, f"兼容桥 listen 无效: {row['listen']!r}"

    peer = make_peer()   # 桥 8N2; 虚拟对上停止位不参与实际成帧, 沿用套件既有口径

    # 旧数据面双向不回归: 上行 (WS → 串口)
    frames = [random.Random(0xC0).randbytes(96) for _ in range(5)]
    hold = threading.Event()
    th, ready, box = ws_send_and_hold(b.ws_url, frames, hold)
    assert ready.wait(10), "旧 /ws 连接超时"
    got = read_exactly(peer, 5 * 96, deadline_s=20)
    assert got == b"".join(frames), "旧 /ws 上行回环破断"
    hold.set()
    th.join(10)
    if box.get("error"):
        raise box["error"]

    # 下行 (串口 → WS 广播)
    payload = random.Random(0xC1).randbytes(128)
    cth, cready, cbox = start_ws_collectors(b.ws_url, 1, len(payload))
    wait_all_ready(cready, cbox)
    serial_write_all(peer, payload)
    payloads = join_collectors(cth, cbox, timeout=30)
    assert payloads[0] == payload, "旧 /ws 下行广播破断"

    # 兼容桥自身的 fleet 数据面端口 (FR-10a) 同样可用: 直发 ws://<listen>/ws → 串口
    push3, done3, pbox3 = ws_push(f"ws://127.0.0.1:{row_port}/ws", [payload])
    got3 = read_exactly(peer, len(payload), deadline_s=15)
    push3.join(10)
    assert done3.wait(5) and not pbox3.get("error"), f"fleet 数据面发射器异常: {pbox3}"
    assert got3 == payload, "兼容桥 fleet 数据面 (listen 端口) 不可用"

    # 控制面上 tap 亦可旁看兼容桥 (FR-10g 对兼容桥同样成立)
    rid = rows[0]["id"]
    tap_url = f"ws://127.0.0.1:{b.http_port}/api/fleet/{rid}/tap"
    tth, tready, tbox = ws_collect_exact(tap_url, payload, allow_text=True)
    assert tready.wait(10), "tap 连接超时"
    serial_write_all(peer, payload)
    tth.join(30)
    if tbox.get("error"):
        raise tbox["error"]
    assert tbox["payload"] == payload, "兼容桥 tap 未收到串口 RX 字节"
