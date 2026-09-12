# -*- coding: utf-8 -*-
"""FR-3 自动重开 — 状态机 Closed→Opening→Open→Retry(re) 必须在状态接口可见。

a) 桥指向不存在端口启动 → phase=="retry" 且 lastError 非空;
b) close 打断 retry (1s 重试间隔内不再拉起);
c) config 换回 COM1 + open 恢复后, 数据面恢复。
"""
from __future__ import annotations

import random
import threading
import time

from conftest import read_exactly, ws_send_and_hold


def test_fr3_reopen_state_machine(start_bridge, make_peer):
    # —— a) 指向不存在的端口启动 → retry + lastError 非空 ——
    b = start_bridge(port_name="COM99", baud=115200, config="8N1")
    deadline = time.monotonic() + 15
    st, ok = None, False
    while time.monotonic() < deadline:
        st = b.status()
        if st.get("phase") == "retry" and st.get("lastError"):
            ok = True
            break
        time.sleep(0.1)
    assert ok, f"15s 内未进入 retry+lastError, 最后 status={st}"
    last_error_a = st["lastError"]

    # —— b) close 打断 retry ——
    code, body = b.post("/api/close", {})
    assert code == 200 and body == {"ok": True}, \
        f"POST /api/close (retry 中) -> {code}: {body!r}"
    st = b.wait_phase("closed", timeout=5)
    time.sleep(2.5)                          # > 1s 重试间隔: 若未打断应已翻回 retry
    st2 = b.status()
    assert st2["phase"] == "closed", \
        f"close 后仍被重试拉起: phase={st2['phase']!r} (retry 未被打断)"

    # —— c) 换回 COM1 并 open, 数据面恢复 ——
    code, body = b.post("/api/config", {"port": "COM1", "baud": 115200,
                                        "dataBits": 8, "parity": "N",
                                        "stopBits": 1, "flow": "none"})
    assert code == 200 and body == {"ok": True}, \
        f"POST /api/config (COM1) -> {code}: {body!r}"
    code, body = b.post("/api/open", {})
    assert code == 200 and body == {"ok": True}, f"POST /api/open -> {code}: {body!r}"
    b.wait_phase("open", timeout=10)

    ser = make_peer(baud=115200, stopbits=1)
    rng = random.Random(0xF3)
    frames = [rng.randbytes(128) for _ in range(10)]
    hold = threading.Event()
    th, ready, box = ws_send_and_hold(b.ws_url, frames, hold)
    assert ready.wait(10)
    got = read_exactly(ser, 10 * 128, deadline_s=30)
    assert got == b"".join(frames), "恢复 COM1 后上行数据面未恢复/内容不一致"
    hold.set()
    th.join(10)
    if box.get("error"):
        raise box["error"]
