# -*- coding: utf-8 -*-
"""PERF 基线 (spec §5) — @perf 标记: 失败不阻塞 Sprint, 但数字必须如实记录。

PERF-1: 921600 baud 回环持续吞吐 ≥ 900 kbps (管道不成为瓶颈);
PERF-2: ≥16 WS 客户端同时广播无错;
PERF-3: 本机 RX→WS 延迟 p95 < 5 ms。
"""
from __future__ import annotations

import asyncio
import math
import struct
import threading
import time

import pytest
import websockets

from conftest import (join_collectors, read_exactly, serial_write_all,
                      start_ws_collectors, wait_all_ready)

pytestmark = pytest.mark.perf

CHUNK = 64 * 1024
TOTAL = 1_000_000  # 每向 1MB


def test_perf_1_throughput_921600(start_bridge, make_peer):
    """PERF-1: 921600 (8N1) 双向回环持续吞吐, 达标线 900 kbps。"""
    b = start_bridge(baud=921600, config="8N1")
    b.wait_phase("open")
    ser = make_peer(baud=921600, timeout=10, write_timeout=120)
    unit = bytes(range(256)) * (CHUNK // 256)          # 64KB 图案块

    # —— 上行: WS → 桥 → COM2 ——
    box: dict = {}

    def _run():
        async def _main():
            async with websockets.connect(b.ws_url, max_size=None) as ws:
                sent = 0
                while sent < TOTAL:
                    n = min(CHUNK, TOTAL - sent)
                    if "t0" not in box:
                        box["t0"] = time.perf_counter()
                    await ws.send(unit[:n])
                    sent += n
        asyncio.run(_main())

    th = threading.Thread(target=_run, daemon=True)
    th.start()
    read_exactly(ser, TOTAL, deadline_s=180)
    t1 = time.perf_counter()
    th.join(30)
    up_kbps = TOTAL * 8 / (t1 - box["t0"]) / 1000

    # —— 下行: COM2 → 桥 → WS ——
    # 台架约束 (非预期放宽): ELTIMA 虚拟对按"写事件"搬运, 单写 > RX 队列(4KB) 的
    # 部分滞留发送侧不再送达 —— 无桥对照实验 (pyserial 直读 COM1) 亦卡在 4096B。
    # 故 2KB/帧 + sleep(1ms)(实测≈1.5ms) ≈ 1.3 MB/s 节流, 远高于 900kbps 达标线,
    # 台架不构成瓶颈; 真实 921600 UART 线速也仅 ≈92KB/s。
    th2, ready2, box2 = start_ws_collectors(b.ws_url, 1, TOTAL)
    wait_all_ready(ready2, box2)
    pace = bytes(range(256)) * 8                     # 2KB
    t0 = time.perf_counter()
    written = 0
    while written < TOTAL:
        n = min(len(pace), TOTAL - written)
        serial_write_all(ser, pace[:n])
        written += n
        time.sleep(0.001)
    payloads = join_collectors(th2, box2, 180)
    t1 = time.perf_counter()
    down_kbps = TOTAL * 8 / (t1 - t0) / 1000
    expected_dn = pace * (TOTAL // len(pace)) + pace[:TOTAL % len(pace)]
    assert payloads[0] == expected_dn, \
        f"下行 1MB 内容不一致 (收 {len(payloads[0])}B / 期望 {TOTAL}B)"

    summary = (f"PERF-1 @921600 8N1: 上行 {up_kbps:.0f} kbps, "
               f"下行 {down_kbps:.0f} kbps, 阈值 900 kbps")
    print(summary)
    assert up_kbps >= 900 and down_kbps >= 900, summary + " —— 未达标"


def test_perf_2_16_clients_broadcast(start_bridge, make_peer):
    """PERF-2: 16 个 WS 客户端同时在线广播, 无错 (全部收齐且内容一致)。"""
    b = start_bridge()
    b.wait_phase("open")
    ser = make_peer(stopbits=2)

    n_frames, size = 100, 256
    frames = [struct.pack(">H", i) * (size // 2) for i in range(n_frames)]
    payload = b"".join(frames)

    th, ready, box = start_ws_collectors(b.ws_url, 16, len(payload))
    wait_all_ready(ready, box)
    time.sleep(0.3)
    for f in frames:
        serial_write_all(ser, f)
        time.sleep(0.005)
    payloads = join_collectors(th, box, 60)
    bad = [i for i, p in enumerate(payloads) if p != payload]
    assert not bad, (
        f"PERF-2: 客户端 {bad} 收流不完整/不一致 "
        f"(各收 {[len(payloads[i]) for i in bad]}B / 期望 {len(payload)}B)")


def test_perf_3_rx_to_ws_latency_p95(start_bridge, make_peer):
    """PERF-3: 串口 RX → WS 送达延迟, p95 < 5 ms (300 样本, 单帧在途法)。"""
    b = start_bridge()
    b.wait_phase("open")
    ser = make_peer(stopbits=2)
    n = 300

    async def _main() -> list[float]:
        lat: list[float] = []
        async with websockets.connect(b.ws_url, max_size=None) as ws:
            for i in range(n):
                frame = struct.pack(">H", i) + b"\x55" * 14     # 16B
                t0 = time.perf_counter()
                await asyncio.to_thread(ser.write, frame)
                # 桥按串口读块切 WS 消息, 一帧可能拆成多条消息 (字节流语义),
                # 按固定帧长重组, 到达时刻 = 帧最后一字节的送达时刻
                got = bytearray()
                while len(got) < 16:
                    got += await asyncio.wait_for(ws.recv(), timeout=10)
                t1 = time.perf_counter()
                assert bytes(got) == frame, f"延迟样本帧不匹配: {bytes(got)!r}"
                lat.append((t1 - t0) * 1000.0)
                await asyncio.sleep(0.003)
        return lat

    lat = sorted(asyncio.run(_main()))
    p95 = lat[math.ceil(0.95 * n) - 1]
    summary = (f"PERF-3 RX→WS 延迟 ms: min={lat[0]:.2f} p50={lat[n // 2]:.2f} "
               f"p95={p95:.2f} max={lat[-1]:.2f} (n={n}, 阈值 p95<5)")
    print(summary)
    assert p95 < 5.0, summary + " —— 未达标"
