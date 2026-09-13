# -*- coding: utf-8 -*-
"""FR-12 自动重连可选项 — QA 语义黑盒套件 (ADR-16①, Sprint 6)。

预期只来自 spec FR-12 + ADR-16① (docs/team/decisions.md), 不读 src/ 调预期:
- 每桥 autoReconnect 默认 true; CLI --reconnect/--no-reconnect + 建桥/改配均可设;
  fleet 桥对象与单桥 /api/status 回显 (契约 12→13 / 14→15 字段, conftest 随动修订)。
- false 时串口断开 → 直接 phase=closed (lastError 注明「自动重连已关闭」),
  不进重试循环 (retries 恒 0 / 不再递增); 手动打开不受影响 (单次可试)。

覆盖矩阵 (任务速记 a-e):
a. --no-reconnect 起桥 (COM99)     → closed (非 retry) + lastError 含「自动重连已关闭」+ retries 恒 0;
b. 默认 (true) 同场景 (守护既有行为) → retry 且 retries 递增 (ADR-15 行为不被 FR-12 破坏);
c. 建桥 body {"autoReconnect": false} → fleet 行/详情回显 false; 默认建桥回显 true;
d. 运行中 PATCH /api/fleet/<id>/config 改 false → 即时生效: retry 态转 closed, 不再重试;
e. false 下手动 open 仍单次可试    → 对真实在位的 COM2 (架构师指令指定; ELTIMA 端点可开),
   POST /api/config 换 COM2 + open → open; 对幽灵 COM99 手动 open 失败 → closed 不 retry;
f. 专项: closed 态 lastError 注明「自动重连已关闭」(启动打开失败 / 手动 open 失败两条路径)。
   注: 注明要求在 a/f 断言 —— 实测 (2026-09-13) 后端仅回 OS 错误文案, 未注明该字样,
   a/f 如实 FAIL 记录为 dev-backend 违约项 (见 qa-sprint6-plan.md §2); 其余语义全过。

串口纪律: COM99 为幽灵端口; COM2 仅 e 用作桥侧单次手动打开验证 (不与 fleet 用例的 COM1
占用互相干扰), 全程禁止碰 COM8。运行: 在项目根执行  python -m pytest tests/test_fr12_reconnect_opt.py -v

后端未落地时经 fr12_ready 探针门控跳过 [BLOCKED-BY-BACKEND]; 落地即全量放行 (即书即跑)。
"""
from __future__ import annotations

import subprocess
import tempfile
import time
from pathlib import Path

import pytest

from conftest import (
    BRIDGE_EXE,
    BRIDGE_COM,
    HTTP_HOST,
    fleet_act_ok, fleet_create, fleet_new_id, fleet_purge, fleet_rows,
    free_tcp_port, http_get, http_patch, kill_all_bridges, row_of, take_port,
    wait_row_phase,
)

CLOSED_NOTE = "自动重连已关闭"   # spec FR-12: lastError 注明该字样


# ------------------------------------------------------------------ 就绪探针

@pytest.fixture(scope="session")
def fr12_ready():
    """FR-12 后端就绪探针 (会话级一次): 单桥 /api/status 无 autoReconnect 字段
    → 整组 [BLOCKED-BY-BACKEND] 跳过。不改任何预期 —— 只区分"实现未到位"与"实现违约";
    后端落地后本探针放行, 套件即书即跑 (沿 fleet_ready 先例)。"""
    kill_all_bridges()
    port = free_tcp_port()
    d = Path(tempfile.mkdtemp(prefix="serialhub_fr12_probe_"))
    cmd = [str(BRIDGE_EXE), "--headless", "--addr", f"{HTTP_HOST}:{port}",
           "--no-fleet", "--no-open"]
    st, proc = None, None
    with open(d / "probe.log", "w", encoding="utf-8") as lf:
        proc = subprocess.Popen(cmd, stdout=lf, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if proc.poll() is not None:
                    break
                try:
                    code, st = http_get(f"http://{HTTP_HOST}:{port}/api/status")
                    if code == 200:
                        break
                    st = None
                except Exception:
                    st = None
                time.sleep(0.1)
        finally:
            if proc.poll() is None:
                subprocess.run(["taskkill", "/F", "/PID", str(proc.pid), "/T"],
                               capture_output=True, check=False)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
    if isinstance(st, dict) and "autoReconnect" in st:
        return True
    pytest.skip(
        "[BLOCKED-BY-BACKEND] FR-12 autoReconnect 契约未实现: /api/status -> "
        f"{st!r}, 进程退出码 {proc.returncode}, 日志: {d / 'probe.log'}")


# ------------------------------------------------------------------ 断言工具

def _assert_note(last_error, ctx: str) -> None:
    assert isinstance(last_error, str) and CLOSED_NOTE in last_error, \
        f"{ctx} lastError 未注明「{CLOSED_NOTE}」(FR-12): {last_error!r}"


def _sample_closed_no_retry(b, seconds: float, ctx: str) -> None:
    """采样窗内: phase 恒 closed (不得 retry/opening), retries 恒 0。
    (lastError 注明「自动重连已关闭」由 test_a / test_f 专项断言, 不在此重复。)"""
    samples = []
    t_end = time.monotonic() + seconds
    while time.monotonic() < t_end:
        st = b.status()
        assert st["phase"] == "closed", \
            f"{ctx}: autoReconnect=false 时不得进入重试循环, phase={st['phase']!r} ({st!r})"
        assert st["retries"] == 0, \
            f"{ctx}: retries 应恒 0 (不进重试循环, FR-12): {st!r}"
        samples.append(st["retries"])
        time.sleep(0.4)
    assert samples, f"{ctx}: 采样窗为空"


def _detail_bridge(detail) -> dict:
    """GET /api/fleet/<id> 详情形状容差: 裸桥对象或 {ok, bridge:{...}} 信封均可。"""
    assert isinstance(detail, dict), f"详情非对象: {detail!r}"
    if isinstance(detail.get("bridge"), dict):
        return detail["bridge"]
    return detail


def _count_increases(samples: list) -> int:
    return sum(1 for a, b in zip(samples, samples[1:]) if b > a)


def _open_accepted(code: int, body, ctx: str) -> None:
    """手动 open 请求受理判据: 2xx 受理 (失败异步落相) 或 4xx+ok:false (同步拒绝)。
    相位语义 (closed/open) 由后续 wait 断言承载, 不锁受理形状。"""
    sync_reject = code >= 400 and isinstance(body, dict) and body.get("ok") is False
    assert code in (200, 201) or sync_reject, f"{ctx} -> {code}: {body!r}"


# ==================================================================
# a) --no-reconnect 起桥 (COM99) → closed 非 retry + 注明 + retries 恒 0
# ==================================================================

def test_a_no_reconnect_flag_failed_open_goes_closed_not_retry(start_bridge, fr12_ready):
    b = start_bridge(port_name="COM99", extra=["--no-reconnect"])
    st = b.status()
    assert st["autoReconnect"] is False, \
        f"--no-reconnect 起桥后 status 回显 autoReconnect 应为 false (FR-12): {st!r}"
    poll = None
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:            # 打开失败落定 (opening → closed)
        poll = b.status()
        if poll["phase"] == "closed" and poll.get("lastError"):
            break
        time.sleep(0.1)
    assert poll is not None and poll["phase"] == "closed", \
        f"8s 内未落定 closed: {poll!r}"
    _assert_note(poll.get("lastError"), "打开失败落定后")

    _sample_closed_no_retry(b, 6.0, "--no-reconnect COM99")


# ==================================================================
# b) 默认 (true) 同场景 → retry 且 retries 递增 (守护 ADR-15 既有行为)
# ==================================================================

def test_b_default_reconnect_keeps_retry_loop(start_bridge, fr12_ready):
    b = start_bridge(port_name="COM99")           # 默认: 不给 --reconnect/--no-reconnect
    st = b.status()
    assert st["autoReconnect"] is True, \
        f"默认回显 autoReconnect 应为 true (FR-12 默认值): {st!r}"
    deadline = time.monotonic() + 15
    st = None
    while time.monotonic() < deadline:            # 守护既有行为: 缺席端口 → retry
        st = b.status()
        if st.get("phase") == "retry" and st.get("lastError"):
            break
        time.sleep(0.1)
    assert st is not None and st.get("phase") == "retry", f"15s 内未进入 retry: {st!r}"

    samples = []
    t_end = time.monotonic() + 5.0                # 短窗守护 (细粒度递增已由 FR-11 套件覆盖)
    while time.monotonic() < t_end:
        st = b.status()
        assert st["phase"] == "retry", f"观察窗内 phase 越出 retry: {st!r}"
        assert st["autoReconnect"] is True, f"retry 期间 autoReconnect 回显漂移: {st!r}"
        samples.append(st["retries"])
        time.sleep(0.4)
    assert _count_increases(samples) >= 2, \
        f"默认 (true) 下 5s retries 递增不足 2 次 (ADR-15 行为被 FR-12 破坏?): {samples}"


# ==================================================================
# c) 建桥 body {"autoReconnect": false} → fleet 行/详情回显 false; 默认 → true
# ==================================================================

def test_c_fleet_create_echoes_auto_reconnect(start_fleet, fleet_ready, fr12_ready):
    api = start_fleet()
    fleet_purge(api)
    port_f = take_port()
    code, resp = fleet_create(api, "qa-fr12-off", "COM99", port_f,
                              extra={"autoReconnect": False})
    assert code in (200, 201) and not (isinstance(resp, dict) and resp.get("ok") is False), \
        f"建桥 (autoReconnect=false) -> {code}: {resp!r}"
    bid = fleet_new_id(api, resp, port_f)

    row = row_of(api, bid)
    assert row is not None, f"建桥后 fleet 行缺失: {fleet_rows(api)}"
    assert row.get("autoReconnect") is False, \
        f"fleet 行未回显 autoReconnect=false (FR-12): {row!r}"
    code, detail = api.get(f"/api/fleet/{bid}")
    assert code == 200 and isinstance(detail, dict), \
        f"GET /api/fleet/{bid} -> {code}: {detail!r}"
    assert _detail_bridge(detail).get("autoReconnect") is False, \
        f"详情未回显 autoReconnect=false (FR-12): {detail!r}"

    # 对照组: 默认建桥 → 回显 true
    port_t = take_port()
    code, resp2 = fleet_create(api, "qa-fr12-def", "COM99", port_t)
    assert code in (200, 201), f"默认建桥 -> {code}: {resp2!r}"
    bid2 = fleet_new_id(api, resp2, port_t)
    row2 = row_of(api, bid2)
    assert row2 is not None, "默认建桥行缺失"
    assert row2.get("autoReconnect") is True, \
        f"默认建桥回显应为 true (FR-12 默认值): {row2!r}"

    fleet_act_ok(api, bid, "delete")
    fleet_act_ok(api, bid2, "delete")


# ==================================================================
# d) 运行中 PATCH /api/fleet/<id>/config 改 false → 即时生效 (retry → closed)
# ==================================================================

def test_d_patch_config_disables_reconnect_live(start_fleet, fleet_ready, fr12_ready,
                                                make_peer):
    api = start_fleet()
    fleet_purge(api)
    blocker = make_peer(port_name=BRIDGE_COM)     # 占住桥侧 COM1 → open 必败 → retry
    port = take_port()
    code, resp = fleet_create(api, "qa-fr12-live", BRIDGE_COM, port)
    assert code in (200, 201), f"建桥 -> {code}: {resp!r}"
    bid = fleet_new_id(api, resp, port)
    fleet_act_ok(api, bid, "start")
    row = wait_row_phase(api, bid, "retry", timeout=20)
    assert row["autoReconnect"] is True, f"进入 retry 前默认应为 true: {row!r}"

    # 运行中改配: PATCH config {"autoReconnect": false} → 即时生效
    code, body = http_patch(f"{api.base}/api/fleet/{bid}/config",
                            {"autoReconnect": False})
    assert code in (200, 201) and not (isinstance(body, dict) and body.get("ok") is False), \
        f"PATCH /api/fleet/{bid}/config -> {code}: {body!r}"

    deadline = time.monotonic() + 10              # retry 态 → closed (不再重试)
    row = None
    while time.monotonic() < deadline:
        row = row_of(api, bid)
        if row is not None and row.get("phase") == "closed":
            break
        time.sleep(0.1)
    assert row is not None and row["phase"] == "closed", \
        f"PATCH 后 10s 内 retry 未转 closed (即时生效违约): {row!r}"
    assert row["autoReconnect"] is False, f"改配后行回显未更新: {row!r}"

    # 生效稳定性: 3s 窗内不再进入 retry, retries 不再递增 (重试循环已停)
    frozen = []
    t_end = time.monotonic() + 3.0
    while time.monotonic() < t_end:
        row = row_of(api, bid)
        assert row is not None, "改配后桥行消失"
        assert row["phase"] == "closed", \
            f"改配后仍试图重试 (phase={row['phase']!r}, FR-12 违约): {row!r}"
        frozen.append(row["retries"])
        time.sleep(0.4)
    assert all(b <= a for a, b in zip(frozen, frozen[1:])), \
        f"改配后 retries 仍在递增 (重试未停): {frozen}"

    fleet_act_ok(api, bid, "delete")


# ==================================================================
# e) false 下手动 open 仍单次可试 (真实在位 COM2: open 成功回 open)
# ==================================================================

def test_e_manual_open_still_works_when_disabled(start_bridge, fr12_ready):
    b = start_bridge(port_name="COM99", extra=["--no-reconnect", "--no-open"])
    b.wait_phase("closed")
    assert b.status()["autoReconnect"] is False

    # 手动 open 幽灵 COM99: 单次尝试失败 → closed (不得转入自动重试)
    code, body = b.post("/api/open", {})
    _open_accepted(code, body, "手动 open (COM99) 请求")
    deadline = time.monotonic() + 8
    st = None
    while time.monotonic() < deadline:
        st = b.status()
        if st["phase"] == "closed" and st.get("lastError"):
            break
        time.sleep(0.1)
    assert st is not None and st["phase"] == "closed", \
        f"手动 open 失败后未回到 closed: {st!r}"
    _sample_closed_no_retry(b, 3.0, "手动 open 失败后")

    # 手动 open 真实在位端口 (COM2, 架构师指令): 单次成功 → open (false 只禁自动重试,
    # 不禁手动打开, FR-12)
    code, body = b.post("/api/config", {"port": "COM2", "baud": 115200,
                                        "dataBits": 8, "parity": "N",
                                        "stopBits": 1, "flow": "none"})
    assert code == 200 and body == {"ok": True}, f"POST /api/config -> {code}: {body!r}"
    code, body = b.post("/api/open", {})
    _open_accepted(code, body, "手动 open (COM2)")
    b.wait_phase("open", timeout=10)
    st = b.status()
    assert st["phase"] == "open" and st["autoReconnect"] is False, \
        f"false 下手动 open 应成功且开关保持 false: {st!r}"


# ==================================================================
# f) 专项: closed 态 lastError 注明「自动重连已关闭」(spec FR-12 明文) —— 覆盖
#    启动打开失败与手动 open 失败两条落 closed 路径
# ==================================================================

def test_f_closed_last_error_notes_reconnect_disabled(start_bridge, fr12_ready):
    # 路径 1: --no-reconnect 起桥 (COM99) 打开失败 → closed
    b = start_bridge(port_name="COM99", extra=["--no-reconnect"])
    deadline = time.monotonic() + 8
    st = None
    while time.monotonic() < deadline:
        st = b.status()
        if st["phase"] == "closed" and st.get("lastError"):
            break
        time.sleep(0.1)
    assert st is not None and st["phase"] == "closed", f"8s 内未落定 closed: {st!r}"
    _assert_note(st.get("lastError"), "启动打开失败后")

    # 路径 2: 手动 open 幽灵端口失败 → closed (同一注明要求)
    b2 = start_bridge(port_name="COM99", extra=["--no-reconnect", "--no-open"])
    b2.wait_phase("closed")
    code, body = b2.post("/api/open", {})
    _open_accepted(code, body, "手动 open (COM99) 请求")
    deadline = time.monotonic() + 8
    st2 = None
    while time.monotonic() < deadline:
        st2 = b2.status()
        if st2["phase"] == "closed" and st2.get("lastError"):
            break
        time.sleep(0.1)
    assert st2 is not None and st2["phase"] == "closed", f"8s 内未落定 closed: {st2!r}"
    _assert_note(st2.get("lastError"), "手动 open 失败后")
