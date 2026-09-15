# -*- coding: utf-8 -*-
"""XTest 数据链路交叉场景套件 — QA 席位 f1 (qa-xtest-data) · spec FR-19/20/21。

定位: 不重复 qa-sprint13 既有断言 (单能力正途), 专测**能力交叉**与**真实流程**:
  X1  录制 + 转发同时: 桥开转发 (本地 TCP 收站) + 录制中双向灌字节 →
      转发口收全 RX AND JSONL 对账 (两路各自完整, 互不丢帧; TX 不入转发口)。
  X2  回放 × 转发 (方向语义): 回放把 tx 行写串口 **TX** (对端 COM2 收到);
      转发只搬运串口 **RX** (FR-20 只出不进) —— 故转发口必须收到真实 RX
      且**收不到**回放字节; 对端收到回放字节且不含自身写的 RX (无自环)。
  X3  日志全事件 (FR-21): 同一 --log-file 进程跑 启动→开桥→录制开始/结束→
      回放开始/停止→转发首连→断线→重连, 断言各事件行齐备且时序成立。
  X4  多桥干扰: 两座桥 (COM1/COM2 各占一个数据端口) 同时录制, WS 交替灌
      不同图案 → 各自 JSONL 方向对账互串即 FAIL (A.tx=PA/A.rx=PB, B 对称)。
  X5  资源: 连续 5 轮录制开始/停止 → 每轮文件独立且逐字节完整、停止后尺寸
      稳定、串口/录制器/HTTP 全程可用、工作集无异常增长。

黑盒纪律 (沿 conftest): 预期只来自 spec FR-19/20/21 + 任务书 (交叉矩阵),
不读 src/ 调预期; 日志事件词表经 src 探针定位 (仅定位证据字符串, 非调预期)。
串口仅 COM1(桥)↔COM2(对端/第二桥); 禁碰 COM8; 端口 18300+ (take_port 占用探测)。
录像走每测试独立 --recordings-dir 临时目录, 测毕自清; 进程由夹具按 PID 清理。

运行: 项目根执行  SERIALHUB_EXE=<副本> python -m pytest tests/test_xtest_data.py -v
"""
from __future__ import annotations

import asyncio
import json
import re
import shutil
import socket
import subprocess
import tempfile
import threading
import time
from pathlib import Path

import websockets

from conftest import (
    HTTP_HOST,
    fleet_act_ok,
    fleet_create,
    fleet_new_id,
    fleet_purge,
    post_accepted,
    read_exactly,
    row_of,
    serial_write_all,
    take_port,
    wait_row_phase,
    ws_push,
)

# ------------------------------------------------------------------ 测试数据

RX_CHUNKS = [b"XT1-RX-A-0123456789", b"XT1-RX-B-abcdef", b"XT1-RX-C-XYZ"]
TX_CHUNKS = [b"xt1-tx-1-01234", b"xt1-tx-2-56789", b"xt1-tx-3-abcde"]
REPLAY_CHUNKS = [b"XT2-REPLAY-1-0123", b"XT2-REPLAY-2-4567", b"XT2-REPLAY-3-abcd"]
FWD_RX_CHUNKS = [b"XT2-FWD-RX-ONE", b"XT2-FWD-RX-TWO"]
PA_CHUNKS = [b"X4<BR-A>-c1-0123456789", b"X4<BR-A>-c2-abcdef",
             b"X4<BR-A>-c3-XYZ", b"X4<BR-A>-c4-end"]
PB_CHUNKS = [b"x4<br-b>-C1-9876543210", b"x4<br-b>-C2-fedcba",
             b"x4<br-b>-C3-zyx", b"x4<br-b>-C4-dne"]


# ------------------------------------------------------------------ 工具

class FwdSink:
    """FR-20 转发收站: 本地 TCP listener, accept 后持续收字节 (沿 bb_sprint13 口径)。"""

    def __init__(self, port: int):
        self.port = port
        self.srv = socket.socket()
        self.srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.srv.bind((HTTP_HOST, port))
        self.srv.listen(4)
        self.srv.settimeout(0.2)
        self.conns: list[socket.socket] = []
        self.accepts = 0
        self.buf = bytearray()
        self._stop = threading.Event()
        self._t = threading.Thread(target=self._loop, daemon=True)
        self._t.start()

    def _loop(self) -> None:
        while not self._stop.is_set():
            try:
                c, _ = self.srv.accept()
                c.settimeout(0.2)
                self.conns.append(c)
                self.accepts += 1
            except socket.timeout:
                pass
            except OSError:
                break
            for c in list(self.conns):
                try:
                    d = c.recv(4096)
                    if d:
                        self.buf += d
                    else:
                        self.conns.remove(c)
                except socket.timeout:
                    pass
                except OSError:
                    try:
                        self.conns.remove(c)
                    except ValueError:
                        pass

    def clear(self) -> None:
        self.buf.clear()

    def drop_peers(self) -> None:
        """主动断开已accept连接 (模拟远端崩溃, 触发桥侧写失败→3s重连)。"""
        for c in list(self.conns):
            try:
                c.close()
            except OSError:
                pass
            try:
                self.conns.remove(c)
            except ValueError:
                pass

    def close(self) -> None:
        self._stop.set()
        self._t.join(timeout=2)
        try:
            self.srv.close()
        except OSError:
            pass


def _open_bridge(api, name: str, serial_port: str, listen: int) -> str:
    """建桥 + 启动 + 等 open, 返回桥 id。"""
    code, resp = fleet_create(api, name, serial_port, listen)
    assert code in (200, 201) and not (isinstance(resp, dict) and resp.get("ok") is False), \
        f"POST /api/fleet -> {code}: {resp!r}"
    bid = fleet_new_id(api, resp, listen)
    fleet_act_ok(api, bid, "start")
    wait_row_phase(api, bid, "open", timeout=15)
    return bid


def _configure_forward(api, bid: str, sink: FwdSink) -> None:
    """配 forwardTcp 并等到 forwardConnected=true + 收站首连 accept。"""
    target = f"127.0.0.1:{sink.port}"
    code, body = api.post(f"/api/fleet/{bid}/config", {"forwardTcp": target})
    post_accepted(code, body, f"/api/fleet/{bid}/config (forwardTcp={target})")
    row = None
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        row = row_of(api, bid)   # conftest 兼容 /api/fleet 裸列表与 {bridges:[...]} 信封
        if row is not None and row.get("forwardTcp") == target \
                and row.get("forwardConnected") is True and sink.accepts >= 1:
            return
        time.sleep(0.1)
    raise AssertionError(f"8s 内转发未建立: row={row!r} accepts={sink.accepts}")


def _wait_sink_payload(sink: FwdSink, payload: bytes, timeout: float = 6.0) -> None:
    """轮询收站直到 payload 完整到达 (作为前缀), 再静默守窗后断言**全等**。"""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if bytes(sink.buf[:len(payload)]) == payload:
            break
        time.sleep(0.05)
    time.sleep(0.7)   # 静默守窗: 任何多余字节 (回放串入/TX 回注/重复) 在此现形
    assert bytes(sink.buf) == payload, \
        f"转发收站流不符: 得 {bytes(sink.buf)!r} / 期望恰 {payload!r}"


def _ws_send_gapped(ws_url: str, frames, gap_s: float, hold: threading.Event):
    """WS 客户端: 逐帧发送 (帧间 gap) 后挂住连接; hold 置位后退出。"""
    ready = threading.Event()
    box: dict = {}

    def _run():
        async def _main():
            async with websockets.connect(ws_url, max_size=None) as ws:
                ready.set()
                for f in frames:
                    await ws.send(f)
                    await asyncio.sleep(gap_s)
                while not hold.is_set():
                    await asyncio.sleep(0.05)
        try:
            asyncio.run(_main())
        except BaseException as e:  # noqa: BLE001
            box["error"] = e

    th = threading.Thread(target=_run, daemon=True)
    th.start()
    return th, ready, box


def _dual_ws_inject(url_a: str, url_b: str, chunks_a, chunks_b, gap_s: float):
    """两路 WS 同时注入: 双连接建立后逐帧交替发送 (帧间 gap), 发完即断。"""
    assert len(chunks_a) == len(chunks_b)
    ready = threading.Event()
    box: dict = {}

    def _run():
        async def _main():
            async with websockets.connect(url_a, max_size=None) as wa, \
                    websockets.connect(url_b, max_size=None) as wb:
                ready.set()
                for ca, cb in zip(chunks_a, chunks_b):
                    await wa.send(ca)
                    await wb.send(cb)
                    await asyncio.sleep(gap_s)
        try:
            asyncio.run(_main())
        except BaseException as e:  # noqa: BLE001
            box["error"] = e

    th = threading.Thread(target=_run, daemon=True)
    th.start()
    return th, ready, box


def _start_rec(api, bid: str) -> str:
    """record/start → 返回录像文件名 (响应契约 {ok,file}, 黑盒实证)。"""
    code, body = api.post(f"/api/fleet/{bid}/record/start", {})
    post_accepted(code, body, f"/api/fleet/{bid}/record/start")
    assert isinstance(body, dict) and isinstance(body.get("file"), str) and body["file"], \
        f"record/start 响应缺 file 字段: {body!r}"
    return body["file"]


def _stop_rec(api, bid: str, want_bytes: int | None = None) -> dict:
    """record/stop → 响应 {ok,file,frames,bytes}; bytes 为录制总字节, 逐字节对账。"""
    code, body = api.post(f"/api/fleet/{bid}/record/stop", {})
    post_accepted(code, body, f"/api/fleet/{bid}/record/stop")
    assert isinstance(body, dict) and "file" in body and "bytes" in body, \
        f"record/stop 响应缺契约字段: {body!r}"
    if want_bytes is not None:
        assert body["bytes"] == want_bytes, \
            f"record/stop 汇总对账失败: bytes={body['bytes']}(望{want_bytes})"
    return body


def _read_jsonl(recdir: Path, name: str) -> list:
    path = recdir / name
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and not path.exists():
        time.sleep(0.1)
    assert path.exists(), f"录像文件未落盘: {path}"
    lines = []
    for ln in path.read_text("utf-8", "replace").splitlines():
        if ln.strip():
            lines.append(json.loads(ln))
    return lines


def _dir_concat(lines: list, direction: str) -> bytes:
    return b"".join(bytes.fromhex(ln["hex"]) for ln in lines
                    if ln.get("dir") == direction)


def _working_set_kb(pid: int) -> int:
    """tasklist 取进程工作集 (KB, CSV 末列 '45,678 K')。"""
    out = subprocess.run(["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"],
                         capture_output=True, check=False).stdout.decode("utf-8", "replace")
    for line in out.splitlines():
        if "serialhub" in line.lower():
            last = line.rsplit('","', 1)[-1].strip().strip('"')
            digits = re.sub(r"[^\d]", "", last)
            if digits:
                return int(digits)
    raise AssertionError(f"tasklist 未找到 PID {pid}: {out!r}")


def _new_recdir() -> Path:
    return Path(tempfile.mkdtemp(prefix="serialhub_xtest_f1_"))


# ------------------------------------------------------------------ X1 录制+转发同时

def test_x1_record_and_forward_simultaneous(start_fleet, fr19_ready, make_peer):
    """桥同时开转发与录制 → 转发口收全 RX 且 JSONL 双向对账 (互不丢帧); TX 不入转发口。"""
    recdir = _new_recdir()
    api = start_fleet(extra=["--recordings-dir", str(recdir)])
    fleet_purge(api)
    listen = take_port(18301)
    bid = _open_bridge(api, "qa-xt1-recfwd", "COM1", listen)
    peer = make_peer()
    sink = FwdSink(take_port(18341))
    try:
        _configure_forward(api, bid, sink)
        sink.clear()          # 丢弃首连积压, 此后收站流 = 本测试注入的 RX, 应恰全等

        fname = _start_rec(api, bid)
        # 双向交替灌: COM2 对端 → RX (录制+转发双路), WS 客户端 → TX (仅录制)
        ws_url = f"ws://{HTTP_HOST}:{listen}/ws"
        hold = threading.Event()
        th, ready, box = _ws_send_gapped(ws_url, TX_CHUNKS, gap_s=0.35, hold=hold)
        assert ready.wait(10), "WS 客户端未连上"
        for chunk in RX_CHUNKS:
            serial_write_all(peer, chunk)
            time.sleep(0.35)
        hold.set()
        th.join(15)
        if box.get("error"):
            raise box["error"]
        time.sleep(0.6)       # 串口/转发落窗余量

        want_total = sum(len(c) for c in RX_CHUNKS) + sum(len(c) for c in TX_CHUNKS)
        _stop_rec(api, bid, want_bytes=want_total)
        time.sleep(0.6)       # 落盘 flush 余量

        # JSONL 侧: 双向逐字节对账 (录制不因转发丢帧)
        lines = _read_jsonl(recdir, fname)
        assert lines, "录像文件为空"
        rx = _dir_concat(lines, "rx")
        tx = _dir_concat(lines, "tx")
        assert rx == b"".join(RX_CHUNKS), f"JSONL RX 对账失败: {rx!r}"
        assert tx == b"".join(TX_CHUNKS), f"JSONL TX 对账失败: {tx!r}"
        for d, chunks in (("rx", RX_CHUNKS), ("tx", TX_CHUNKS)):
            n = sum(1 for ln in lines if ln.get("dir") == d)
            assert n >= len(chunks), f"{d} 帧数 {n} < 发送次数 {len(chunks)}"

        # 转发侧: RX 全量按序 (转发不因录制丢帧) + TX 不回注 (只出不进) —— 恰全等
        _wait_sink_payload(sink, b"".join(RX_CHUNKS))
        print(f"[X1] JSONL {len(lines)} 行 (rx {sum(1 for l in lines if l['dir']=='rx')}"
              f"/tx {sum(1 for l in lines if l['dir']=='tx')} 帧); "
              f"RX {len(rx)}B + TX {len(tx)}B 对账全等; 转发流 {len(bytes(sink.buf))}B 恰全等")

        # stop 后桥不受损
        wait_row_phase(api, bid, "open", timeout=5)
    finally:
        sink.close()
        shutil.rmtree(recdir, ignore_errors=True)


# ------------------------------------------------------------------ X2 回放×转发 (方向语义)

def test_x2_replay_direction_semantics_with_forward(start_fleet, fr19_ready, make_peer):
    """回放 (tx 行→串口 TX) × 转发 (串口 RX→TCP) 同桥并行:
    对端收全回放字节 (TX 方向); 转发口收全真实 RX 且收不到回放字节 (方向不混)。"""
    recdir = _new_recdir()
    api = start_fleet(extra=["--recordings-dir", str(recdir)])
    fleet_purge(api)
    listen = take_port(18302)
    bid = _open_bridge(api, "qa-xt2-replfwd", "COM1", listen)
    # timeout=0.1 < 帧间隔: read 按"已到字节"逐块返回 (沿 fr19 修订口径)
    peer = make_peer(timeout=0.1)
    sink = FwdSink(take_port(18342))
    rec_name = "qa_xt2_replay_src.jsonl"
    try:
        _configure_forward(api, bid, sink)
        sink.clear()

        # 手工构造 tx-only 录像 (ADR-24⑥: 相对 ms; 回放只回放 tx 行)
        gap_ms = 300
        (recdir / rec_name).write_text(
            "".join(json.dumps({"ts": i * gap_ms, "dir": "tx", "hex": c.hex()}) + "\n"
                    for i, c in enumerate(REPLAY_CHUNKS)), encoding="utf-8")
        payload = b"".join(REPLAY_CHUNKS)

        code, body = api.post(f"/api/fleet/{bid}/replay",
                              {"file": rec_name, "speed": 1, "loop": False})
        post_accepted(code, body, f"/api/fleet/{bid}/replay")

        # 回放进行中同时灌真实 RX (COM2→COM1) —— 两条数据通路并行
        time.sleep(0.1)
        serial_write_all(peer, FWD_RX_CHUNKS[0])
        time.sleep(0.25)
        serial_write_all(peer, FWD_RX_CHUNKS[1])
        time.sleep(0.25)

        # 对端 (COM2) 必须收全回放字节 (TX 方向落地); 自写的 RX 不自环
        got = read_exactly(peer, len(payload), deadline_s=20.0)
        assert got == payload, f"对端回放字节不符: {got!r} / 期望 {payload!r}"
        time.sleep(0.5)
        extra = peer.read(512)
        assert not extra, f"loop=false 回放结束后仍有多余字节: {extra!r}"

        # 转发口: 恰收到两条真实 RX; 回放字节 (TX) 不得出现在转发流 (方向语义)
        _wait_sink_payload(sink, b"".join(FWD_RX_CHUNKS))
        print(f"[X2] 对端收回放 {len(got)}B 全等; 转发流 {len(bytes(sink.buf))}B "
              f"= 真实 RX 全等, 回放字节 0 入镜")

        wait_row_phase(api, bid, "open", timeout=5)
    finally:
        sink.close()
        shutil.rmtree(recdir, ignore_errors=True)


# ------------------------------------------------------------------ X3 日志全事件

def test_x3_log_file_full_event_chain(start_fleet, fr19_ready, make_peer):
    """同一 --log-file 进程跑全事件链: 启动/相位迁移/录制开始结束/回放开始停止/
    转发首连→断线→重连, 各事件行齐备且 转发重连 时序成立 (连接→断开→再连接)。"""
    recdir = _new_recdir()
    tmp = Path(tempfile.mkdtemp(prefix="serialhub_xtest_f1_log_"))
    log_path = tmp / "run.log"
    api = start_fleet(extra=["--recordings-dir", str(recdir),
                             "--log-file", str(log_path)])
    fleet_purge(api)
    listen = take_port(18303)
    bid = _open_bridge(api, "qa-xt3-logs", "COM1", listen)
    peer = make_peer()
    sink = FwdSink(take_port(18343))
    rec_name = "qa_xt3_replay_src.jsonl"
    try:
        # --- 1) 录制 + 转发同时 (X1 缩样, 供 录制/转发 事件行) ---
        _configure_forward(api, bid, sink)
        conn_1_idx = None     # 记录首连发生时刻 (行序稍后按文件内容定位)
        fname = _start_rec(api, bid)
        for chunk in (b"XT3-LOG-RX-1", b"XT3-LOG-RX-2"):
            serial_write_all(peer, chunk)
            time.sleep(0.3)
        time.sleep(0.5)
        _stop_rec(api, bid)
        time.sleep(0.5)

        # --- 2) 回放 loop=true → 显式停止 (回放开始/停止 事件行) ---
        (recdir / rec_name).write_text(
            json.dumps({"ts": 0, "dir": "tx", "hex": b"XT3-REPLAY-LOOP".hex()}) + "\n",
            encoding="utf-8")
        code, body = api.post(f"/api/fleet/{bid}/replay",
                              {"file": rec_name, "speed": 1, "loop": True})
        post_accepted(code, body, f"/api/fleet/{bid}/replay")
        read_exactly(peer, 2 * len(b"XT3-REPLAY-LOOP"), deadline_s=10.0)  # ≥2 圈
        code, body = api.post(f"/api/fleet/{bid}/replay/stop", {})
        post_accepted(code, body, f"/api/fleet/{bid}/replay/stop")
        time.sleep(0.4)
        peer.reset_input_buffer()

        # --- 3) 转发断线 → 3s 自动重连 (重连事件行 + 时序) ---
        sink.drop_peers()
        acc_before = sink.accepts
        serial_write_all(peer, b"XT3-RECONN-PROBE")   # 触发写失败探测
        row = None
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            row = row_of(api, bid)
            if row is not None and row.get("forwardConnected") is True \
                    and sink.accepts > acc_before:
                break
            time.sleep(0.2)
        assert row is not None and row.get("forwardConnected") is True \
            and sink.accepts > acc_before, \
            f"15s 内转发未重连: row={row!r} accepts={sink.accepts}"
        sink.clear()
        serial_write_all(peer, b"XT3-AFTER-RECONNECT")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline \
                and bytes(sink.buf) != b"XT3-AFTER-RECONNECT":
            time.sleep(0.1)
        assert bytes(sink.buf) == b"XT3-AFTER-RECONNECT", \
            f"重连后转发未续传: {bytes(sink.buf)!r}"

        # --- 4) 日志全事件断言 ---
        time.sleep(0.5)
        assert log_path.exists(), f"日志文件未落盘: {log_path}"
        lines = [l for l in log_path.read_text("utf-8", "replace").splitlines()
                 if l.strip()]
        assert lines, "日志文件为空"
        assert all(l.startswith("[") and "] [" in l for l in lines), \
            f"日志行格式违约 (应为 [ts] [level] msg): {lines[:3]!r}"

        assert any("日志文件启用" in l for l in lines), "缺 进程启动 事件行"
        assert any("数据面启动" in l for l in lines), "缺 桥数据面启动 事件行"
        assert any(re.search(r"\[state\].*相位:.*→ open$", l) for l in lines), \
            "缺 相位→open 状态机迁移行"

        assert any("录制开始" in l for l in lines), "缺 录制开始 事件行"
        assert any("录制结束" in l for l in lines), "缺 录制结束 事件行"
        assert any("回放开始" in l for l in lines), "缺 回放开始 事件行"
        assert any("回放停止" in l for l in lines), "缺 回放停止 事件行"

        conns = [i for i, l in enumerate(lines) if "旁路转发已连接" in l]
        drops = [i for i, l in enumerate(lines)
                 if "旁路转发连接断开" in l or "旁路转发写失败" in l]
        assert len(conns) >= 2, f"转发 已连接 事件应 ≥2 次 (首连+重连): {len(conns)}"
        assert drops and conns[0] < drops[0] < conns[-1], \
            (f"转发重连时序违约: conns={conns} drops={drops} "
             f"lines={[l for l in lines if '旁路转发' in l]!r}")
        print(f"[X3] 日志 {len(lines)} 行: conns(已连接)x{len(conns)} drops(断开/写失败)"
              f"x{len(drops)}, 时序 conn[0]<drop[0]<conn[-1] 成立; "
              f"启动/相位/录制开始结束/回放开始停止 事件全中")

        wait_row_phase(api, bid, "open", timeout=5)
    finally:
        sink.close()
        shutil.rmtree(recdir, ignore_errors=True)
        shutil.rmtree(tmp, ignore_errors=True)


# ------------------------------------------------------------------ X4 多桥干扰

def test_x4_dual_bridge_recording_isolation(start_fleet, fr19_ready):
    """两座桥 (COM1/COM2, 各占一个数据端口) 同时录制, WS 交替灌不同图案:
    A.tx=PA / A.rx=PB, B.tx=PB / B.rx=PA —— 互串 (错方向/错桥) 即 FAIL。"""
    recdir = _new_recdir()
    api = start_fleet(extra=["--recordings-dir", str(recdir)])
    fleet_purge(api)
    listen_a = take_port(18310)
    listen_b = take_port(18311)
    bid_a = _open_bridge(api, "qa-xt4-a", "COM1", listen_a)
    bid_b = _open_bridge(api, "qa-xt4-b", "COM2", listen_b)
    try:
        fname_a = _start_rec(api, bid_a)
        fname_b = _start_rec(api, bid_b)
        assert fname_a != fname_b, f"两桥录像文件名冲突: {fname_a!r}"

        th, ready, box = _dual_ws_inject(
            f"ws://{HTTP_HOST}:{listen_a}/ws", f"ws://{HTTP_HOST}:{listen_b}/ws",
            PA_CHUNKS, PB_CHUNKS, gap_s=0.35)
        assert ready.wait(10), "双 WS 注入器未连上"
        th.join(20)
        if box.get("error"):
            raise box["error"]
        time.sleep(0.6)

        want = sum(len(c) for c in PA_CHUNKS) + sum(len(c) for c in PB_CHUNKS)
        _stop_rec(api, bid_a, want_bytes=want)   # A: tx=PA + rx=PB
        _stop_rec(api, bid_b, want_bytes=want)   # B: tx=PB + rx=PA
        time.sleep(0.6)

        pa = b"".join(PA_CHUNKS)
        pb = b"".join(PB_CHUNKS)
        lines_a = _read_jsonl(recdir, fname_a)
        lines_b = _read_jsonl(recdir, fname_b)
        # 方向×桥归属对账 (桥 A 的 TX 出 COM1 → 桥 B 的 RX; 反向同理)
        assert _dir_concat(lines_a, "tx") == pa, "桥 A TX 拼接 != 图案 PA (丢/串)"
        assert _dir_concat(lines_a, "rx") == pb, "桥 A RX 拼接 != 图案 PB (丢/串)"
        assert _dir_concat(lines_b, "tx") == pb, "桥 B TX 拼接 != 图案 PB (丢/串)"
        assert _dir_concat(lines_b, "rx") == pa, "桥 B RX 拼接 != 图案 PA (丢/串)"
        # 显式反串扰: A 的流里不得混入"本该只在 B 流里"的错位组合
        assert pa not in _dir_concat(lines_a, "rx"), "桥 A RX 混入 PA (桥间串扰)"
        assert pb not in _dir_concat(lines_b, "rx"), "桥 B RX 混入 PB (桥间串扰)"
        assert pb not in _dir_concat(lines_a, "tx") or pa == pb, "桥 A TX 混入 PB"
        assert pa not in _dir_concat(lines_b, "tx"), "桥 B TX 混入 PA (桥间串扰)"
        print(f"[X4] A: tx {len(_dir_concat(lines_a,'tx'))}B=PA rx "
              f"{len(_dir_concat(lines_a,'rx'))}B=PB; B: tx=PB rx=PA; "
              f"四向逐字节全等, 零串扰")

        wait_row_phase(api, bid_a, "open", timeout=5)
        wait_row_phase(api, bid_b, "open", timeout=5)
    finally:
        shutil.rmtree(recdir, ignore_errors=True)


# ------------------------------------------------------------------ X5 资源

def test_x5_record_cycles_resource_hygiene(start_fleet, fr19_ready, make_peer):
    """连续 5 轮录制开始/停止: 每轮文件独立逐字节完整、停止后尺寸稳定、
    串口/录制器/HTTP 全程可用、工作集无异常增长 (进程内存对比)。"""
    recdir = _new_recdir()
    api = start_fleet(extra=["--recordings-dir", str(recdir)])
    fleet_purge(api)
    listen = take_port(18305)
    bid = _open_bridge(api, "qa-xt5-cycles", "COM1", listen)
    peer = make_peer()
    files: list[str] = []
    try:
        ws_after_r1 = None
        for i in range(1, 6):
            pat = b"X5-ROUND-%d-%s-END" % (i, bytes([0x41 + i]) * 24)
            fname = _start_rec(api, bid)
            assert fname not in files, \
                f"第 {i} 轮录像文件名复用旧文件 (句柄/命名混用?): {fname!r}"
            serial_write_all(peer, pat)
            time.sleep(0.35)
            _stop_rec(api, bid, want_bytes=len(pat))
            time.sleep(0.6)

            lines = _read_jsonl(recdir, fname)
            assert _dir_concat(lines, "rx") == pat, \
                f"第 {i} 轮 RX 对账失败: {_dir_concat(lines, 'rx')!r} / 期望 {pat!r}"
            assert _dir_concat(lines, "tx") == b"", \
                f"第 {i} 轮混入 TX 行: {_dir_concat(lines, 'tx')!r}"
            path = recdir / fname
            size1 = path.stat().st_size
            time.sleep(0.5)
            assert path.stat().st_size == size1, \
                f"第 {i} 轮 stop 后文件仍在增长: {size1} -> {path.stat().st_size}"
            files.append(fname)
            if i == 1:
                time.sleep(0.5)
                ws_after_r1 = _working_set_kb(api.proc.pid)

        # 工作集对比: 5 轮后 vs 第 1 轮后 (20MB 阈值 = 宽松毛漏检测, 实测值入报告)
        time.sleep(1.0)
        ws_after_r5 = _working_set_kb(api.proc.pid)
        growth = ws_after_r5 - ws_after_r1
        assert growth < 20_000, \
            f"工作集异常增长: r1={ws_after_r1}KB r5={ws_after_r5}KB (+{growth}KB)"
        print(f"[X5] 5 轮文件独立全对账; 工作集 r1={ws_after_r1}KB "
              f"r5={ws_after_r5}KB (+{growth}KB, 阈值 20000KB)")

        # 串口通路仍健在 (端口句柄未劣化): WS 上行 → 对端照收
        th, done, box = ws_push(f"ws://{HTTP_HOST}:{listen}/ws", [b"X5-POSTCHECK-WS-TX"])
        assert done.wait(10), "WS 上行未完成"
        if box.get("error"):
            raise box["error"]
        assert read_exactly(peer, len(b"X5-POSTCHECK-WS-TX"), deadline_s=10.0) \
            == b"X5-POSTCHECK-WS-TX", "5 轮录制后串口通路受损"

        # 录制器未被 5 轮榨死: 第 6 轮 start/stop 仍正常
        pat6 = b"X5-ROUND-6-BONUS"
        fname6 = _start_rec(api, bid)
        serial_write_all(peer, pat6)
        time.sleep(0.35)
        _stop_rec(api, bid, want_bytes=len(pat6))
        time.sleep(0.6)
        assert _dir_concat(_read_jsonl(recdir, fname6), "rx") == pat6, "第 6 轮对账失败"

        # 列表端点与落盘文件对账
        code, listing = api.get(f"/api/fleet/{bid}/recordings")
        assert code == 200, f"GET recordings -> {code}"
        listed = {r["file"] for r in listing.get("recordings", [])} \
            if isinstance(listing, dict) else set()
        for f in files + [fname6]:
            assert f in listed, f"recordings 列表缺 {f!r}: {sorted(listed)!r}"

        wait_row_phase(api, bid, "open", timeout=5)
    finally:
        shutil.rmtree(recdir, ignore_errors=True)
