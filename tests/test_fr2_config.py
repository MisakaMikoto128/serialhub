# -*- coding: utf-8 -*-
"""FR-2 串口配置 — CLI 与 Web 双入口可配; 参数真实生效用回环验证。

spec FR-2: 波特率 110~2,000,000、数据位 7/8、校验 N/E/O、停止位 1/2、流控;
CLI 与 Web 双入口可配。ADR-5 ③: POST 成功 {"ok":true}, 失败 400+{"ok":false,"error"}。
7E1 用字节 ≤0x7F 图案。
"""
from __future__ import annotations

import asyncio
import random
import threading
import time

import pytest
import websockets

from conftest import read_exactly, serial_write_all, ws_send_and_hold

# (config 令牌, baud, pyserial 对端参数 (bytesize, parity, stopbits))
CASES = [
    ("8N1", 115200, (8, "N", 1)),
    ("8N2", 115200, (8, "N", 2)),
    ("7E1", 115200, (7, "E", 1)),
    ("8N1", 921600, (8, "N", 1)),
    ("8N1", 2000000, (8, "N", 1)),
]


def _pattern(rng: random.Random, n: int, ascii7: bool) -> bytes:
    if ascii7:
        return bytes(rng.randrange(0, 0x80) for _ in range(n))   # 7E1: 字节 ≤0x7F
    return rng.randbytes(n)


@pytest.mark.parametrize("cfg,baud,peerparams", CASES,
                         ids=[f"{c}-{b}" for c, b, _ in CASES])
def test_fr2_config_matrix(start_bridge, make_peer, cfg, baud, peerparams):
    """CLI 配置 8N1/8N2/7E1/921600/2M 各一遍; status 回显 + 双向回环字节级一致。"""
    b = start_bridge(baud=baud, config=cfg)
    bs, par, sb = peerparams
    ser = make_peer(baud=baud, bytesize=bs, parity=par, stopbits=sb)

    st = b.status()
    assert st["baud"] == baud, f"status.baud={st['baud']!r}, 期望 {baud}"
    assert st["config"] == cfg, f"status.config={st['config']!r}, 期望 {cfg}"
    b.wait_phase("open")

    rng = random.Random(hash((cfg, baud)) & 0xFFFF)
    pat = _pattern(rng, 512, ascii7=(cfg == "7E1"))

    # 回环: WS 发 pat → 对端按本配置读 pat → 对端回灌 → WS 收回 pat
    hold = threading.Event()
    box: dict = {}

    def _run():
        async def _main():
            async with websockets.connect(b.ws_url, max_size=None) as ws:
                hold.wait(10)
                await ws.send(pat)
                echo = bytearray()
                while len(echo) < len(pat):
                    msg = await asyncio.wait_for(ws.recv(), timeout=20)
                    echo += msg
                box["echo"] = bytes(echo)
        try:
            asyncio.run(_main())
        except BaseException as e:  # noqa: BLE001
            box["error"] = e

    th = threading.Thread(target=_run, daemon=True)
    th.start()
    time.sleep(0.2)                          # 等 WS 连接建立
    hold.set()
    up = read_exactly(ser, len(pat), deadline_s=30)
    assert up == pat, f"[{cfg}@{baud}] 上行不一致: 收 {len(up)}B, 首差异偏移 " \
        f"{next((i for i, (x, y) in enumerate(zip(up, pat)) if x != y), 'N/A(长度不足)')}"
    serial_write_all(ser, up)                # 回灌
    th.join(30)
    if "error" in box:
        raise box["error"]
    assert box["echo"] == pat, (
        f"[{cfg}@{baud}] 下行回环不一致: 收 {len(box['echo'])}B / 期望 {len(pat)}B")


def test_fr2_config_via_web_api(start_bridge, make_peer):
    """Web 入口: --no-open 启动 → POST /api/config + /api/open → 参数生效 (FR-2/FR-5)。"""
    b = start_bridge(extra=["--no-open"])
    st0 = b.status()
    assert st0["phase"] == "closed", \
        f"未给 --port 且 --no-open 时 phase={st0['phase']!r}, 期望 closed (FR-5)"

    code, body = b.post("/api/config", {"port": "COM1", "baud": 115200,
                                        "dataBits": 8, "parity": "N",
                                        "stopBits": 2, "flow": "none"})
    assert code == 200, f"POST /api/config -> {code}: {body!r}"
    assert body == {"ok": True}, f"ADR-5 ③: 成功应恰 {{'ok': True}}, 实得 {body!r}"

    code, body = b.post("/api/open", {})
    assert code == 200 and body == {"ok": True}, f"POST /api/open -> {code}: {body!r}"
    st = b.wait_phase("open")
    assert st["baud"] == 115200 and st["config"] == "8N2", \
        f"API 配置未生效: baud={st['baud']!r} config={st['config']!r}"

    ser = make_peer(baud=115200, stopbits=2)
    rng = random.Random(0xA2)
    frames = [rng.randbytes(64) for _ in range(4)]
    hold = threading.Event()
    th, ready, box = ws_send_and_hold(b.ws_url, frames, hold)
    assert ready.wait(10)
    got = read_exactly(ser, 256, deadline_s=30)
    assert got == b"".join(frames), "API 入口配置后数据面不通/内容不一致"
    hold.set()
    th.join(10)
    if box.get("error"):
        raise box["error"]


@pytest.mark.parametrize("field,bad", [
    ("baud", 3),          # 低于 110
    ("baud", 2000001),    # 高于 2,000,000
    ("dataBits", 5),      # 只许 7/8
    ("parity", "X"),      # 只许 N/E/O
    ("stopBits", 3),      # 只许 1/2
])
def test_fr2_config_api_rejects_out_of_range(start_bridge, field, bad):
    """spec FR-2 范围之外必须拒绝, 且失败形状为 400 + {"ok":false,"error"} (ADR-5 ③)。"""
    b = start_bridge(extra=["--no-open"])
    body = {"port": "COM1", "baud": 115200, "dataBits": 8,
            "parity": "N", "stopBits": 1, "flow": "none"}
    body[field] = bad
    code, resp = b.post("/api/config", body)
    assert code == 400, f"{field}={bad!r} 超出 FR-2 范围应 400, 实得 {code}: {resp!r}"
    assert isinstance(resp, dict) and resp.get("ok") is False and resp.get("error"), \
        f"ADR-5 ③ 失败形状不符: {resp!r}"


def test_fr2_cli_rejects_invalid(start_bridge):
    """CLI 非法参数应拒绝启动 (进程退出码非 0), 而不是带着坏参数跑起来。"""
    for extra in (["--config", "8X9"], ["--baud", "3"]):
        b = start_bridge(port_name="COM99", extra=extra, wait=False)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and b.proc.poll() is None:
            time.sleep(0.1)
        code = b.proc.poll()
        note = ""
        if code is None:
            try:
                note = f" (进程仍存活, status={b.status()})"
            except Exception:
                note = " (进程仍存活, HTTP 未就绪)"
        assert code is not None and code != 0, \
            f"CLI {extra} 未被拒绝{note}"
