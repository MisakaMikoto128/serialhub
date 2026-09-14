# -*- coding: utf-8 -*-
"""FR-19 数据录制与回放黑盒套件 — Sprint 13 批次 B (ADR-24⑤ B1) / spec FR-19。

覆盖 (预期只来自 spec FR-19 + 任务契约 + ADR-24⑥ 裁定, 不读 src/ 调预期):
1  录制: open 桥 record/start → COM2 对端灌已知字节 (RX) + WS 客户端发已知字节 (TX) →
   record/stop → exe 旁 recordings/ JSONL 行校验: 契约三键 ts/dir/hex 齐备 (多键以
   warning 记偏差不挡门)、ts 非负整数 ms 单调不减且为「录制起点起的相对毫秒」
   (ADR-24⑥ 裁定, 2026-09-15 解锁时随动修订: 原按 qa-plan §3 A1 双语义宽容断言
   「近当前时刻」, 现绝对 epoch 即违约)、dir∈{rx,tx}、hex 合法;
   按 dir 拼接与发送字节逐字节对账 (硬契约), 每方向帧数 ≥ 发送次数 (软下限: 实现
   可按读写块拆并帧, 字节不丢即可); 录制起点前的流量不得入镜 (start 边界)。
2  停止语义: record/stop 后继续灌 RX —— 无新文件、原文件大小不变。
3  回放前置: 缺席串口桥 (COM99, phase=retry 永不 open) replay → 400/409。
4  回放正途: 手工构造 TX-only 已知录像 (契约行格式, ADR-24⑥ 相对 ms) →
   replay(speed=1, loop=false) → COM2 对端按序收全字节; 时序宽容断言 (预期跨度 0.6s:
   实测跨度 ≥ 0.25×预期, 总耗时 ≤ 预期+8s, CI 慢 runner 不误伤); loop=false 自然结束
   (静默守窗无多余字节)。测量方法修订 (2026-09-15 解锁时, 非预期放宽): 原对端
   read timeout=1.5s > 回放全程 0.6s, 单次 read 吞掉整个 pacing 窗 → spread 恒测 0
   的伪影 (探针实证后端 pacing 正常); 改短超时 0.1s 逐读块计时后 spread 可分辨。
5  回放安全: file="../Cargo.toml" 与反斜杠变体 → 400 (路径穿越); 不存在文件 → 4xx。

契约留白 (如实记录, 不堵门, 详见 docs/team/reports/qa-sprint13-plan.md §3;
其中 A1 ts 语义 / A2 rx 行回放语义已经 ADR-24⑥ 裁定, 2026-09-15 解锁时随动修订):
- 回放源 ts 语义 —— ADR-24⑥ 已裁定: 录制开始起的相对毫秒 (原 A1 双语义宽容撤销);
- 回放对 rx 行的处理 —— ADR-24⑥ 已裁定: 只回放 tx 行 (rx 不回注), 源文件仅含 tx 行;
- record/start|stop / recordings / replay 响应体形状未定 —— 只断状态码;
- 回放假设接受 recordings/ 内契约格式的录像文件; 若实现要求录像先经 record 登记,
  R4 失败后改 record→replay 回环并回填裁定。

后端未落地时经 fr19_ready 门控整组跳过 [BLOCKED-BY-BACKEND] (见 conftest)。
运行: 在项目根执行  python -m pytest tests/test_fr19_record_replay.py -v
"""
from __future__ import annotations

import asyncio
import json
import threading
import time
import warnings

import websockets

from conftest import (
    BRIDGE_COM,
    HTTP_HOST,
    delete_recordings,
    fleet_act_ok,
    fleet_create,
    fleet_new_id,
    fleet_purge,
    list_recordings,
    post_accepted,
    recordings_dir,
    serial_write_all,
    take_port,
    wait_row_phase,
)

RX_CHUNKS = [b"QA-FR19-RX-A-0123456789", b"QA-FR19-RX-B-abcdef", b"QA-FR19-RX-C-XYZ"]
TX_CHUNKS = [b"qa-fr19-tx-1-01234", b"qa-fr19-tx-2-56789", b"qa-fr19-tx-3-abcde"]


def _open_bridge(api, name: str, serial_port: str, listen: int) -> str:
    """建桥 + 启动 + 等 open, 返回桥 id (listen 传实值, 勿用常量 —— take_port 可能退临时口)。"""
    code, resp = fleet_create(api, name, serial_port, listen)
    assert code in (200, 201) and not (isinstance(resp, dict) and resp.get("ok") is False), \
        f"POST /api/fleet -> {code}: {resp!r}"
    bid = fleet_new_id(api, resp, listen)
    fleet_act_ok(api, bid, "start")
    wait_row_phase(api, bid, "open", timeout=15)
    return bid


def _ws_send_gapped(ws_url: str, frames, gap_s: float, hold: threading.Event):
    """WS 客户端: 逐帧发送 (帧间隔 gap_s, 供帧数对账) 后挂住连接; hold 置位后退出。"""
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
        except BaseException as e:  # noqa: BLE001 — 异常原样带回主线程
            box["error"] = e

    th = threading.Thread(target=_run, daemon=True)
    th.start()
    return th, ready, box


def _read_jsonl(names) -> list:
    lines = []
    for name in sorted(names):
        text = (recordings_dir() / name).read_text("utf-8", "replace")
        for ln in text.splitlines():
            if ln.strip():
                lines.append(json.loads(ln))
    return lines


# ------------------------------------------------------------------ 1/2 录制

def test_fr19_record_jsonl_shape_and_accounting(start_fleet, fr19_ready, make_peer):
    api = start_fleet()
    fleet_purge(api)
    listen = take_port(18200)
    bid = _open_bridge(api, "qa-fr19-rec", BRIDGE_COM, listen)
    peer = make_peer()
    new_files: set = set()
    try:
        # start 边界: 录制前的流量不得入镜
        serial_write_all(peer, b"PRE-REC-BOUNDARY")
        time.sleep(0.4)
        before = list_recordings()
        code, body = api.post(f"/api/fleet/{bid}/record/start", {})
        post_accepted(code, body, f"/api/fleet/{bid}/record/start")

        # 双向灌已知字节: WS 客户端 → TX (写回串口), COM2 对端 → RX (桥读串口)
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

        code, body = api.post(f"/api/fleet/{bid}/record/stop", {})
        post_accepted(code, body, f"/api/fleet/{bid}/record/stop")
        time.sleep(0.5)                       # 落盘 flush 余量

        new_files = list_recordings() - before
        assert new_files, (
            f"recordings/ 未出现新录像文件 (契约位置: exe 旁 {recordings_dir()}); "
            f"现目录: {sorted(list_recordings())!r}")
        lines = _read_jsonl(new_files)
        assert lines, "录像文件为空 (无任何 JSONL 行)"

        # 行形状 (spec FR-19: 每行 {"ts":ms,"dir":"rx|tx","hex":"..."})
        for ln in lines:
            missing = {"ts", "dir", "hex"} - set(ln)
            assert not missing, f"JSONL 行缺契约键 {sorted(missing)}: {ln!r}"
            extra = set(ln) - {"ts", "dir", "hex"}
            if extra:
                warnings.warn(
                    f"FR-19 录像行含契约外键 {sorted(extra)} (spec 只钉三键, 记偏差): {ln!r}",
                    stacklevel=1)
            assert isinstance(ln["ts"], int) and not isinstance(ln["ts"], bool) \
                and ln["ts"] >= 0, \
                f"ts 应为非负整数 ms (ADR-24⑥: 录制起点起相对毫秒, 首帧可为 0): {ln!r}"
            assert ln["dir"] in ("rx", "tx"), f"dir 越域: {ln!r}"
            assert isinstance(ln["hex"], str) and len(ln["hex"]) > 0, \
                f"hex 应非空字符串: {ln!r}"
            bytes.fromhex(ln["hex"])          # 合法 hex (偶长), 否则当场抛

        # ADR-24⑥ (2026-09-15 随动修订): ts = 录制开始起的相对毫秒, 绝对 epoch 即违约;
        # 录制窗 ~2s, 2min 上限对慢 runner 仍宽松
        ts = [ln["ts"] for ln in lines]
        assert ts == sorted(ts), "ts 非单调不减"
        assert 0 <= ts[0] < 120_000, \
            f"ts[0] 应为录制起点附近的相对毫秒 (ADR-24⑥): {ts[0]} (绝对 epoch 即违约)"
        assert ts[-1] - ts[0] < 120_000, \
            f"ts 跨度超出录制窗 (>2min): {ts[0]}..{ts[-1]}"

        # 逐字节对账 (硬契约) + 起点边界
        rx = b"".join(bytes.fromhex(ln["hex"]) for ln in lines if ln["dir"] == "rx")
        tx = b"".join(bytes.fromhex(ln["hex"]) for ln in lines if ln["dir"] == "tx")
        assert rx == b"".join(RX_CHUNKS), "RX 拼接 != 对端发送字节 (丢/改/串)"
        assert tx == b"".join(TX_CHUNKS), "TX 拼接 != WS 客户端发送字节 (丢/改/串)"
        assert b"PRE-REC-BOUNDARY" not in rx, "录制起点前的流量入镜 (start 边界失效)"

        # 帧数对账 (软下限: 实现可按读写块拆并, 字节不丢即可)
        n_rx = sum(1 for ln in lines if ln["dir"] == "rx")
        n_tx = sum(1 for ln in lines if ln["dir"] == "tx")
        assert n_rx >= len(RX_CHUNKS), \
            f"RX 帧数 {n_rx} < 发送次数 {len(RX_CHUNKS)} (帧合并过度?)"
        assert n_tx >= len(TX_CHUNKS), \
            f"TX 帧数 {n_tx} < 发送次数 {len(TX_CHUNKS)} (帧合并过度?)"

        # 列表端点可见
        code, listing = api.get(f"/api/fleet/{bid}/recordings")
        assert code == 200, f"GET recordings -> {code}: {str(listing)[:300]!r}"
        raw = json.dumps(listing, ensure_ascii=False)
        assert any(name in raw for name in new_files), \
            f"GET recordings 未包含本次录像文件 {sorted(new_files)!r}: {str(listing)[:300]}"

        # 停止后桥不受损
        wait_row_phase(api, bid, "open", timeout=5)
    finally:
        delete_recordings(new_files)


def test_fr19_record_stop_halts_file_growth(start_fleet, fr19_ready, make_peer):
    api = start_fleet()
    fleet_purge(api)
    bid = _open_bridge(api, "qa-fr19-recstop", BRIDGE_COM, take_port(18200))
    peer = make_peer()
    new_files: set = set()
    try:
        before = list_recordings()
        code, body = api.post(f"/api/fleet/{bid}/record/start", {})
        post_accepted(code, body, f"/api/fleet/{bid}/record/start")
        serial_write_all(peer, b"QA-STOP-CHK-1")
        time.sleep(0.4)
        code, body = api.post(f"/api/fleet/{bid}/record/stop", {})
        post_accepted(code, body, f"/api/fleet/{bid}/record/stop")
        time.sleep(0.5)

        new_files = list_recordings() - before
        assert new_files, "录制期间未产生录像文件"
        sizes = {n: (recordings_dir() / n).stat().st_size for n in new_files}
        captured = b"".join(bytes.fromhex(ln["hex"]) for ln in _read_jsonl(new_files)
                            if ln.get("dir") == "rx")
        assert b"QA-STOP-CHK-1" in captured, "录制期间的 RX 未入镜"

        # stop 后继续灌 RX —— 不得再有落盘
        serial_write_all(peer, b"QA-STOP-CHK-2-AFTER-STOP")
        time.sleep(1.2)
        files2 = list_recordings() - before
        assert files2 == new_files, \
            f"stop 后出现新录像文件: {sorted(files2 - new_files)!r}"
        sizes2 = {n: (recordings_dir() / n).stat().st_size for n in files2}
        assert sizes2 == sizes, f"stop 后录像文件仍在增长: {sizes} -> {sizes2}"
    finally:
        delete_recordings(new_files)


# ------------------------------------------------------------------ 3 回放前置

def test_fr19_replay_rejected_when_serial_not_open(start_fleet, fr19_ready):
    api = start_fleet()
    fleet_purge(api)
    listen = take_port(18203)
    code, resp = fleet_create(api, "qa-fr19-com99", "COM99", listen)
    assert code in (200, 201) and not (isinstance(resp, dict) and resp.get("ok") is False), \
        f"建 COM99 桥失败: {code}: {resp!r}"
    bid = fleet_new_id(api, resp, listen)
    fleet_act_ok(api, bid, "start")
    row = wait_row_phase(api, bid, "retry", timeout=15)   # 缺席串口 → FR-3 retry, 永不 open
    assert row["phase"] == "retry", f"COM99 桥应停在 retry: {row!r}"
    code, body = api.post(f"/api/fleet/{bid}/replay",
                          {"file": "qa_any.jsonl", "speed": 1, "loop": False})
    assert code in (400, 409), \
        f"串口未 open 时 replay 应 400/409: {code}: {str(body)[:300]!r}"


# ------------------------------------------------------------------ 4 回放正途

def test_fr19_replay_delivers_in_order_paced_until_natural_end(
        start_fleet, fr19_ready, make_peer):
    api = start_fleet()
    fleet_purge(api)
    listen = take_port(18201)
    bid = _open_bridge(api, "qa-fr19-replay", BRIDGE_COM, listen)
    rec_name = "qa_fr19_replay_src.jsonl"
    chunks = [b"REPLAY-CHUNK-1-01234", b"REPLAY-CHUNK-2-56789", b"REPLAY-CHUNK-3-abcde"]
    gap_ms = 300
    payload = b"".join(chunks)
    rec_dir = recordings_dir()
    rec_dir.mkdir(parents=True, exist_ok=True)
    # ADR-24⑥ 随动修订 (2026-09-15): ts = 录制起点起的相对毫秒 → 源文件按 0 基相对
    # ms 构造 (0/300/600; 行间差值语义下等价); 仅 tx 行 (ADR-24⑥: 回放只回放 tx 行)
    (rec_dir / rec_name).write_text(
        "".join(json.dumps({"ts": i * gap_ms,
                            "dir": "tx", "hex": c.hex()}) + "\n"
                for i, c in enumerate(chunks)), encoding="utf-8")
    try:
        # timeout=0.1 < 帧间隔 0.3s: read 按"已到字节"逐块返回, pacing 才可分辨
        # (原 timeout=1.5s 单读吞窗伪影, 2026-09-15 修订, 见模块 docstring)
        peer = make_peer(timeout=0.1)
        code, body = api.post(f"/api/fleet/{bid}/replay",
                              {"file": rec_name, "speed": 1, "loop": False})
        post_accepted(code, body, f"/api/fleet/{bid}/replay")

        t0 = time.monotonic()
        buf = bytearray()
        arrivals: list[float] = []            # 每个非空读块到达时刻
        deadline = t0 + 20
        while len(buf) < len(payload) and time.monotonic() < deadline:
            chunk = peer.read(len(payload) - len(buf))
            if chunk:
                arrivals.append(time.monotonic())
                buf += chunk
        assert bytes(buf) == payload, \
            f"对端按序收到的回放字节不符: 得 {bytes(buf)!r} / 期望 {payload!r}"

        expected = gap_ms / 1000.0 * (len(chunks) - 1)     # speed=1, 预期跨度 0.6s
        spread = arrivals[-1] - arrivals[0]
        assert spread >= 0.25 * expected, \
            f"回放未按原始时序 (近瞬发): 跨度 {spread:.3f}s, 预期≈{expected:.2f}s"
        assert (arrivals[-1] - t0) <= expected + 8.0, \
            f"回放总耗时 {arrivals[-1] - t0:.2f}s 超出 CI 容差 (预期≈{expected:.2f}s + 8s)"

        # loop=false 自然结束: 静默守窗无多余字节
        time.sleep(0.5)
        extra = peer.read(512)
        assert not extra, f"loop=false 回放结束后仍有多余字节: {extra!r}"
    finally:
        delete_recordings([rec_name])


# ------------------------------------------------------------------ 5 回放安全

def test_fr19_replay_rejects_path_traversal(start_fleet, fr19_ready):
    api = start_fleet()
    fleet_purge(api)
    bid = _open_bridge(api, "qa-fr19-trav", BRIDGE_COM, take_port(18202))
    for bad in ("../Cargo.toml", "..\\..\\Cargo.toml"):
        code, body = api.post(f"/api/fleet/{bid}/replay",
                              {"file": bad, "speed": 1, "loop": False})
        assert code == 400, \
            f"路径穿越 {bad!r} 应 400: {code}: {str(body)[:300]!r}"
    code, body = api.post(f"/api/fleet/{bid}/replay",
                          {"file": "qa_no_such_rec.jsonl", "speed": 1, "loop": False})
    assert 400 <= code < 500, \
        f"不存在的录像文件应 4xx: {code}: {str(body)[:300]!r}"
