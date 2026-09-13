# -*- coding: utf-8 -*-
"""FR-10b 持久化恢复 — fleet.json 变更即存, 进程重启自动恢复全部桥。

契约 (spec FR-10b + 任务契约): --fleet 指定路径; 建两桥 → taskkill /F 强杀 (不走优雅停机,
验证"变更即存"而非"退出时才存") → 同 --fleet 重启 → 两桥自动恢复 phase=open 且
listen 端点不变 (FR-10f), 数据面可用。

后端未实现时经 fleet_ready 门控跳过 [BLOCKED-BY-BACKEND] (见 conftest)。
"""
from __future__ import annotations

import random
import tempfile
from pathlib import Path

from conftest import (
    BRIDGE_COM,
    HTTP_HOST,
    PEER_COM,
    fleet_act_ok,
    fleet_purge,
    fleet_rows,
    hard_kill,
    listen_port_of,
    read_exactly,
    row_of,
    take_port,
    wait_port_free,
    wait_row_phase,
    wait_serial_free,
    ws_push,
)

from test_fr10_fleet import _start_open_bridge


def test_fr10b_fleet_json_survives_hard_kill(start_fleet, fleet_ready, make_peer):
    fleet_file = Path(tempfile.mkdtemp(prefix="serialhub_fr10_persist_")) / "fleet.json"
    api = start_fleet(fleet_path=fleet_file)
    fleet_purge(api)                     # 清掉播种桥, 清单里只剩本测试的两桥
    port_a = take_port()
    port_b = take_port()
    _start_open_bridge(api, "qa-persist-a", BRIDGE_COM, port_a)   # COM1
    _start_open_bridge(api, "qa-persist-b", PEER_COM, port_b)     # COM2

    assert fleet_file.exists() and fleet_file.stat().st_size > 0, \
        "FR-10b 变更即存: --fleet 清单文件未落盘"

    hard_kill(api)                                  # 断电式强杀, 无优雅停机机会
    wait_port_free(port_a)
    wait_port_free(port_b)

    api2 = start_fleet(fleet_path=fleet_file)       # 同 --fleet 重启
    rows = fleet_rows(api2)
    assert len(rows) == 2, f"重启后应自动恢复两桥: {rows!r}"
    restored = sorted(listen_port_of(r["listen"]) for r in rows)
    assert restored == sorted([port_a, port_b]), \
        f"FR-10f 端点稳定: 恢复后 listen 变化 {restored} != {[port_a, port_b]}"

    wait_row_phase(api2, port_a, "open", timeout=20)   # 两桥自动恢复 open
    wait_row_phase(api2, port_b, "open", timeout=20)

    # 数据面可用: B 占着 COM2, 先停 B 释放对端, 再经 A (COM1) 做回环验证
    row_b = row_of(api2, port_b)
    assert row_b is not None and "id" in row_b, f"B 桥行缺失 id: {rows!r}"
    fleet_act_ok(api2, row_b["id"], "stop")
    wait_row_phase(api2, row_b["id"], "closed", timeout=10)
    wait_serial_free(PEER_COM)                      # stop 后句柄异步释放, 轮询到可开

    peer = make_peer()                              # COM2 → 桥A(COM1) 的回环对端
    payload = random.Random(0xF10).randbytes(640)
    push, done, pbox = ws_push(f"ws://{HTTP_HOST}:{port_a}/ws", [payload])
    got = read_exactly(peer, len(payload), deadline_s=20)
    push.join(10)
    assert done.wait(5) and not pbox.get("error"), f"发射器异常: {pbox}"
    assert got == payload, "恢复桥 A 的数据面不可用/数据不一致"
