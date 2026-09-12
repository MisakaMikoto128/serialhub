# -*- coding: utf-8 -*-
"""FR-1 数据管道 — 黑盒一致性。

spec FR-1: /ws 只传原始二进制帧, 双向; 串口 RX 广播给所有已连客户端;
任意客户端的帧经队列 (FIFO) 串行写入串口; 多客户端并发 TX 不 panic、不丢帧序。
硬约束 3: RX 广播给所有客户端; TX 队列串行化。
"""
from __future__ import annotations

import asyncio
import random
import struct
import threading
import time

import websockets

from conftest import (join_collectors, read_exactly, serial_write_all,
                      start_ws_collectors, wait_all_ready, ws_send_and_hold)

FRAME_N = 100
FRAME_SIZE = 256


def test_fr1_integrity(start_bridge, make_peer):
    """256B 随机图案 ×100 帧, 双向 (WS→串口 与 串口→WS) 字节级一致。"""
    b = start_bridge()                       # COM1 / 115200 / 8N2
    b.wait_phase("open")
    ser = make_peer(baud=115200, stopbits=2)

    rng = random.Random(0xC0FFEE)
    up_frames = [rng.randbytes(FRAME_SIZE) for _ in range(FRAME_N)]
    up_payload = b"".join(up_frames)

    # —— 上行: WS 客户端 → 桥 → COM2 对端 ——
    # 发送端发完后挂住连接, 等主线程把串口侧读完再断开, 排除关闭竞态。
    hold = threading.Event()
    th, ready, box = ws_send_and_hold(b.ws_url, up_frames, hold)
    assert ready.wait(10), "WS 发送端连接超时"
    got = read_exactly(ser, len(up_payload), deadline_s=60)
    assert got == up_payload, (
        f"上行 WS→串口 字节不一致: 收 {len(got)}B / 期望 {len(up_payload)}B, "
        f"首个差异偏移 "
        f"{next((i for i, (x, y) in enumerate(zip(got, up_payload)) if x != y), 'N/A(长度不足)')}")
    hold.set()
    th.join(10)
    if box.get("error"):
        raise box["error"]

    # —— 下行: COM2 对端 → 桥 → WS 客户端 ——
    dn_frames = [rng.randbytes(FRAME_SIZE) for _ in range(FRAME_N)]
    dn_payload = b"".join(dn_frames)
    th2, ready2, box2 = start_ws_collectors(b.ws_url, 1, len(dn_payload))
    wait_all_ready(ready2, box2)
    time.sleep(0.2)
    # 台架约束 (非预期放宽): ELTIMA 虚拟对按"写事件"搬运数据, 单写超过 RX 队列
    # (4KB) 的部分滞留发送侧, 无后续写事件则不送达 —— 无桥对照实验 (pyserial 直读
    # COM1) 同样卡在 4096B。真实 UART 在 115200 下本就按线速逐字节到达, 故按帧节流
    # 写入, 与真机行为一致; 字节级完整性预期不变。
    for f in dn_frames:
        serial_write_all(ser, f)
        time.sleep(0.005)
    payloads = join_collectors(th2, box2, 60)
    assert payloads[0] == dn_payload, (
        f"下行 串口→WS 字节不一致: 收 {len(payloads[0])}B / 期望 {len(dn_payload)}B")


def test_fr1_broadcast(start_bridge, make_peer):
    """3 个 WS 客户端同时在线, COM2 对端发的数据三端都收到且内容一致。"""
    b = start_bridge()
    b.wait_phase("open")
    ser = make_peer(baud=115200, stopbits=2)

    rng = random.Random(0xB0AD)
    n, size = 40, 64
    frames = [i.to_bytes(2, "big") + rng.randbytes(size - 2) for i in range(n)]
    payload = b"".join(frames)

    th, ready, box = start_ws_collectors(b.ws_url, 3, len(payload))
    wait_all_ready(ready, box)
    time.sleep(0.2)                          # 让服务端订阅/计数稳定
    for f in frames:
        serial_write_all(ser, f)
        time.sleep(0.015)                    # 15ms 间隔: 允许逐帧广播, 共 0.6s
    payloads = join_collectors(th, box, 30)
    for i, p in enumerate(payloads):
        assert p == payload, (
            f"客户端{i} 广播收流不一致: 收 {len(p)}B / 期望 {len(payload)}B")


TX_FRAME = 16
TAG_A, TAG_B = 0xA1, 0xB2


def _tx_frame(tag: int, seq: int) -> bytes:
    """固定 16B 帧: tag(1B) + seq(4B BE) + 魔数 0x55AA + 9B 填充 —— 帧内带序号。"""
    return struct.pack(">BI", tag, seq) + b"\x55\xAA" + bytes(9)


def test_fr1_tx_arbitration(start_bridge, make_peer):
    """2 客户端各并发发 50 帧 (带序号), COM2 按序收齐、帧不交叠不丢 (FIFO 仲裁)。"""
    b = start_bridge()
    b.wait_phase("open")
    ser = make_peer(baud=115200, stopbits=2)

    async def _send(tag: int, url: str):
        async with websockets.connect(url, max_size=None) as ws:
            for i in range(50):
                await ws.send(_tx_frame(tag, i))

    async def _main(url: str):
        await asyncio.gather(_send(TAG_A, url), _send(TAG_B, url))

    th = threading.Thread(target=lambda: asyncio.run(_main(b.ws_url)), daemon=True)
    th.start()

    raw = read_exactly(ser, 100 * TX_FRAME, deadline_s=60)
    th.join(10)
    order: dict[int, list[int]] = {TAG_A: [], TAG_B: []}
    for off in range(0, len(raw), TX_FRAME):
        chunk = raw[off:off + TX_FRAME]
        tag, seq = struct.unpack(">BI", chunk[:5])
        assert chunk[5:7] == b"\x55\xAA", (
            f"偏移 {off}: 帧损坏/交叠 (魔数错): {chunk.hex()}")
        assert tag in order, f"偏移 {off}: 未知 tag {tag:#x}"
        order[tag].append(seq)
    for tag, seqs in order.items():
        assert seqs == list(range(50)), (
            f"tag {tag:#x} 帧序/帧数异常: 共 {len(seqs)} 帧, 前 10 = {seqs[:10]}")
