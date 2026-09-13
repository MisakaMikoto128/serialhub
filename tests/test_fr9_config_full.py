# -*- coding: utf-8 -*-
"""Sprint 3 FR-9 配置全量双入口 — 黑盒增补。

FR-9b: --max-clients (WS 超限 Close(1013,"max clients"), API 解除);
FR-9a: POST /api/restart 自我重启 E2E (GUI 专用, 参数保留, headless 400 守卫);
FR-2 对等补齐 (ADR-10): --flow CLI 回环验证 + 非法值拒启。
"""
from __future__ import annotations

import asyncio
import json
import subprocess
import time
import urllib.error
import urllib.request

import websockets

from conftest import free_tcp_port, kill_all_bridges, read_exactly

EXE = r"C:\Users\liuyu\Desktop\WorkPlace\serialhub\target\release\serialhub.exe"
S3_OLD_LOG = r"C:\Users\liuyu\AppData\Local\Temp\qa_fix\s3_old.log"


def http(port, path, body=None, timeout=3.0):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=data,
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode())
    except Exception as e:
        return None, repr(e)


def wait_status(port, timeout=15.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        code, body = http(port, "/api/status", timeout=1.5)
        if code == 200:
            return body
        time.sleep(0.2)
    return None


def serialhub_pids():
    tl = subprocess.run(["tasklist", "/FI", "IMAGENAME eq serialhub.exe"],
                        capture_output=True)
    out = tl.stdout.decode("gbk", "replace")
    return [int(parts[1]) for line in out.splitlines()
            if (parts := line.split()) and parts[0].lower() == "serialhub.exe"]


def test_fr9b_max_clients(start_bridge):
    """FR-9b: --max-clients 1 → 第 1 条 accepted, 第 2 条 Close(1013,"max clients") 且不计入
    clients; POST /api/config {"maxClients":0} 解除后新连接成功。"""
    b = start_bridge(max_clients=1)
    b.wait_phase("open")
    st = b.status()
    assert st["maxClients"] == 1, f"status.maxClients 应为 1: {st['maxClients']!r}"

    async def _main(url):
        """全流程单事件循环: ws1 保持打开跨断言 (asyncio.run 结束即断连)。"""
        out = {"problems": []}
        ws1 = await websockets.connect(url, max_size=None)
        out["ws1_open"] = True
        ws2 = await websockets.connect(url, max_size=None)   # 升级完成, 服务端随即 Close
        try:
            await asyncio.wait_for(ws2.recv(), timeout=5)
            out["ws2_close"] = "unexpected_frame"
        except websockets.exceptions.ConnectionClosed as e:
            out["ws2_code"] = e.rcvd.code if e.rcvd else None
            out["ws2_reason"] = e.rcvd.reason if e.rcvd else None
        except asyncio.TimeoutError:
            out["ws2_close"] = "no_close_5s"
        try:
            await ws2.close()
        except Exception:
            pass
        st = await asyncio.to_thread(http, b.http_port, "/api/status")
        out["clients_after_reject"] = st[1]["clients"] if st[0] == 200 else st
        # 解除限制 → 新连接成功且保持 OPEN
        out["unset_http"] = await asyncio.to_thread(
            http, b.http_port, "/api/config", {"maxClients": 0})
        st = await asyncio.to_thread(http, b.http_port, "/api/status")
        out["maxClients_after_unset"] = st[1]["maxClients"] if st[0] == 200 else st
        ws3 = await websockets.connect(url, max_size=None)
        try:
            await asyncio.wait_for(ws3.recv(), timeout=1.5)
            out["ws3"] = "unexpected_frame"
        except asyncio.TimeoutError:
            out["ws3"] = "OPEN"                              # 未收 Close = accepted
        except websockets.exceptions.ConnectionClosed as e:
            out["ws3"] = f"CLOSED({e.rcvd.code if e.rcvd else '?'})"
        st = await asyncio.to_thread(http, b.http_port, "/api/status")
        out["clients_after_unset"] = st[1]["clients"] if st[0] == 200 else st
        await ws1.close()
        try:
            await ws3.close()
        except Exception:
            pass
        return out

    res = asyncio.run(_main(b.ws_url))
    assert res["ws1_open"] is True, "第 1 条 WS 应被接受"
    assert res.get("ws2_code") == 1013, \
        f"第 2 条 WS 应收 Close(1013): 实得 {res.get('ws2_code')!r}"
    assert res.get("ws2_reason") == "max clients", \
        f"Close reason 应为 'max clients': {res.get('ws2_reason')!r}"
    assert res.get("clients_after_reject") == 1, \
        f"被拒连接不应计入 clients: {res.get('clients_after_reject')!r}"
    assert res.get("unset_http") == (200, {"ok": True}), \
        f"解除限制应 200 {{'ok':true}}: {res.get('unset_http')!r}"
    assert res.get("maxClients_after_unset") == 0, \
        f"解除后 maxClients 应为 0: {res.get('maxClients_after_unset')!r}"
    assert res.get("ws3") == "OPEN", \
        f"解除后新连接应保持 OPEN: {res.get('ws3')!r}"
    assert res.get("clients_after_unset") == 2, \
        f"解除后 clients 应为 2 (旧 1 + 新 1): {res.get('clients_after_unset')!r}"


def test_fr9a_restart_e2e(make_peer):
    """FR-9a: GUI 实例自我重启 (COM2, 8081→8085, max-clients=3, --flow xonxoff):
    旧 ≤3s 退; 新实例 phase=open 且 baud/config/maxClients 保留; WS→串口数据面恢复
    (flow 保留经 COM1 对端逐字节验证)。"""
    peer = make_peer(port_name="COM1", baud=115200, parity="N")    # 对端 COM1 (桥在 COM2)
    log = open(S3_OLD_LOG, "w")
    p_old = subprocess.Popen([EXE, "--port", "COM2", "--addr", "127.0.0.1:8081",
                              "--max-clients", "3", "--flow", "xonxoff"],
                             stdout=log, stderr=subprocess.STDOUT)
    try:
        st = wait_status(8081)
        assert st and st["phase"] == "open", f"旧实例应 open: {st!r}"
        assert st["maxClients"] == 3, f"maxClients 应为 3: {st['maxClients']!r}"
        time.sleep(1.0)                                       # WebView 控制台自连

        data = json.dumps({"addr": "127.0.0.1:8085"}).encode()
        req = urllib.request.Request("http://127.0.0.1:8081/api/restart", data=data,
                                     headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=3) as r:
            code, body = r.status, json.loads(r.read().decode())
        assert code == 200 and body == {"ok": True}, \
            f"POST /api/restart 应 200 {{'ok':true}}: {code} {body!r}"

        try:
            ec_old = p_old.wait(timeout=3)
            old_exited, old_code = True, ec_old
        except subprocess.TimeoutExpired:
            subprocess.run(["taskkill", "/F", "/PID", str(p_old.pid), "/T"], capture_output=True)
            old_exited, old_code = False, None
        assert old_exited, "旧进程应 ≤3s 内退出"

        # 新实例在 8085 接管
        new_st = wait_status(8085, timeout=6)
        assert new_st, "6s 内新实例未在 8085 就绪"
        assert new_st["phase"] == "open", f"新实例 phase 应 open: {new_st!r}"
        assert new_st["port"] == "COM2", f"port 应保留 COM2: {new_st['port']!r}"
        assert new_st["baud"] == 115200, f"baud 应保留: {new_st['baud']!r}"
        assert new_st["config"] == "8N2", f"config 应保留: {new_st['config']!r}"
        assert new_st["maxClients"] == 3, f"maxClients 应保留 3: {new_st['maxClients']!r}"

        # 新进程身份: 恰 1 个 serialhub 进程且非旧 PID
        pids = serialhub_pids()
        assert len(pids) == 1 and pids[0] != p_old.pid, \
            f"应有恰 1 个新 serialhub 进程: {pids} (旧 {p_old.pid})"

        # 数据面恢复 + flow 保留 (xonxoff 到串口): WS 发可打印图案 → COM1 对端逐字节收到
        pattern = bytes((0x20 + (i % 0x5f)) for i in range(256))   # 避开 XON(0x11)/XOFF(0x13)
        async def _send():
            async with websockets.connect("ws://127.0.0.1:8085/ws", max_size=None) as ws:
                await ws.send(pattern)
                await asyncio.sleep(0.5)
        asyncio.run(_send())
        got = read_exactly(peer, len(pattern), deadline_s=20)
        assert got == pattern, (
            f"重启后 WS→串口 数据面/flow 未恢复: 首差异偏移 "
            f"{next((i for i, (x, y) in enumerate(zip(got, pattern)) if x != y), '长度不足')}")
    finally:
        kill_all_bridges()
        time.sleep(0.8)


def test_fr9a_restart_headless_guard(start_bridge):
    """FR-9a: headless 下 POST /api/restart → 400 含 '重启进程', 服务不退出。"""
    b = start_bridge()
    b.wait_phase("open")
    st0 = b.status()
    code, body = b.post("/api/restart", {"addr": "127.0.0.1:8087"})
    assert code == 400, f"headless 下 /api/restart 应 400: {code} {body!r}"
    assert isinstance(body, dict) and body.get("ok") is False \
        and "重启进程" in str(body.get("error")), \
        f"400 错误应含 '重启进程': {body!r}"
    time.sleep(2.0)
    st1 = b.status()                                          # 服务不退: status 仍可访问
    assert st1["phase"] == st0["phase"], f"headless 服务不应退出: {st1!r}"


def test_fr2_flow_xonxoff_loopback(start_bridge, make_peer):
    """FR-2 对等 (ADR-10): --flow xonxoff CLI 起桥, WS→串口回环逐字节 (图案避开 XON/XOFF)。"""
    b = start_bridge(flow="xonxoff")
    b.wait_phase("open")
    peer = make_peer(baud=115200, parity="N")                 # 对端 COM2
    pattern = bytes((0x20 + (i % 0x5f)) for i in range(512))  # 可打印 512B, 避开 0x11/0x13

    async def _send():
        async with websockets.connect(b.ws_url, max_size=None) as ws:
            await ws.send(pattern)
            await asyncio.sleep(0.5)
    asyncio.run(_send())
    got = read_exactly(peer, len(pattern), deadline_s=30)
    assert got == pattern, (
        f"--flow xonxoff 回环字节不一致: 首差异偏移 "
        f"{next((i for i, (x, y) in enumerate(zip(got, pattern)) if x != y), '长度不足')}")


def test_fr2_flow_bogus_rejected():
    """FR-2 对等 (ADR-10): --flow bogus 拒绝启动 (退出码非 0)。"""
    port = free_tcp_port()
    p = subprocess.Popen([EXE, "--headless", "--port", "COM1", "--addr",
                          f"127.0.0.1:{port}", "--flow", "bogus"],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and p.poll() is None:
        time.sleep(0.1)
    code = p.poll()
    assert code is not None and code != 0, \
        f"--flow bogus 应拒绝启动 (进程仍运行, code={code})"
