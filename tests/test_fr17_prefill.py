# -*- coding: utf-8 -*-
"""Sprint 8 QA — FR-17 新建桥端口自动递增预填 (decisions.md ADR-19② / spec FR-17)。

契约 (spec FR-17): 新建桥表单的「网址端口」预填下一个空闲端口 —— 管理台端口+1 起
向上探测, 跳过已用桥端口; 用户仍可手动改。前端行为, 以 Playwright 无头 Chromium 对
真后端黑盒断言 (沿 tools/ui_pixel_audit.js 的 NODE_PATH + require 先例, 见
tests/playwright_prefill.cjs: 只开弹窗读 #npListen, 不提交)。

场景 (管理台端口记 P, 优先 8090):
  a. 空清单 → 预填 = P+1 (首个空闲);
  b. 建两桥占 P+1 / P+2 → 预填 = P+3 (向上跳过已用);
  c. 删掉中间桥 (P+1) → 预填回填空洞 = P+1。

门控 (沿 fleet_ready 先例, 区分"未到位"与"违约"):
- fleet_ready: FR-10 控制面未实现 → 整组 [BLOCKED-BY-BACKEND] 跳过;
- `fr17_admin` 探针读一次预填: 值为空 → 前端预填未落地, 整组
  SKIPPED [BLOCKED-BY-FRONTEND] (dev-ui 落地即自动放行); 有值则三场景违约式验收。
- Playwright 不可用 → SKIPPED [BLOCKED-BY-TOOLING] (安装见 playwright skill:
  npm install -g @playwright/cli@latest + npx playwright install chromium)。

端口/串口: 管理台优先 8090 (被占退回, 且保证 P..P+3 均可绑定); 桥数据口 = P+1..P+3;
串口只用 COM1 (fleet_create 必填串口; 两桥并存时第二座 phase=retry 不影响预填,
预填跳过的判据是"已用桥端口"即 listen, 不依赖串口状态)。全程禁碰 COM8。

运行: 项目根执行  python -m pytest tests/test_fr17_prefill.py -v
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import tempfile
from pathlib import Path

import pytest

from conftest import (BRIDGE_EXE, Bridge, HTTP_HOST, fleet_create, fleet_new_id,
                      fleet_purge, fleet_rows, kill_all_bridges, listen_port_of,
                      take_port, wait_port_free)

PW_SCRIPT = Path(__file__).resolve().parent / "playwright_prefill.cjs"
PREFILL_WAIT_SETTLE_S = 60.0   # node 单次读值上限 (浏览器冷启动余量)


# ------------------------------------------------------------------ 工具

def _npm_global_root() -> str | None:
    """npm 全局 node_modules 路径 (playwright 先例: NODE_PATH 注入, 见 ui_pixel_audit.js)。

    Windows 下 Python subprocess 找 npm 须用 npm.cmd; 找不到时退回
    %APPDATA%\\npm\\node_modules (npm 默认全局根)。
    """
    for cmd in (["npm.cmd", "root", "-g"], ["npm", "root", "-g"]):
        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
            if r.returncode == 0 and (r.stdout or "").strip():
                return (r.stdout or "").strip().splitlines()[-1]
        except Exception:
            continue
    appdata = os.environ.get("APPDATA")
    if appdata:
        cand = Path(appdata) / "npm" / "node_modules"
        if cand.is_dir():
            return str(cand)
    return None


def read_prefill(url: str) -> str:
    """开真浏览器读一次新建桥弹窗的「网址端口」预填值 (playwright_prefill.cjs)。"""
    env = dict(os.environ)
    node_root = _npm_global_root()
    if node_root:
        env["NODE_PATH"] = node_root
    r = subprocess.run(["node", str(PW_SCRIPT), url], capture_output=True,
                       text=True, timeout=PREFILL_WAIT_SETTLE_S, env=env)
    lines = [ln for ln in (r.stdout or "").strip().splitlines() if ln.strip()]
    assert lines, (
        f"playwright_prefill.cjs 无输出 (rc={r.returncode}) stderr={r.stderr[-400:]!r}")
    try:
        payload = json.loads(lines[-1])
    except json.JSONDecodeError:
        raise AssertionError(f"探针输出非 JSON: {lines[-1]!r} stderr={r.stderr[-400:]!r}")
    assert payload.get("ok") is True, f"预填探针失败: {payload}"
    return str(payload.get("prefill", ""))


def pick_admin_base() -> int:
    """管理台端口: 优先任务指定 8090; 退回自由端口且保证 P..P+3 当前均可绑定
    (预填场景要用 P+1..P+3, 避免撞临时端口)。"""
    candidates = [8090] + [take_port() for _ in range(20)]
    for p in candidates:
        socks = []
        try:
            for off in range(4):
                s = socket.socket()
                s.bind((HTTP_HOST, p + off))
                socks.append(s)
            return p
        except OSError:
            pass
        finally:
            for s in socks:
                s.close()
    raise AssertionError("20 个候选基址均无法保证 P..P+3 可绑定")


# ------------------------------------------------------------------ 夹具

@pytest.fixture
def fr17_admin(fleet_ready):
    """真后端管理台 (headless, 临时 fleet 清单, 端口优先 8090) + 预填读取器。

    会话首读兼门控: 预填为空 → [BLOCKED-BY-FRONTEND] 整组跳过。
    teardown: 清清单 + 杀进程 + kill_all_bridges 双保险。
    """
    port = pick_admin_base()
    fleet_path = Path(tempfile.mkdtemp(prefix="serialhub_fr17_")) / "fleet.json"
    cmd = [str(BRIDGE_EXE), "--headless", "--addr", f"{HTTP_HOST}:{port}",
           "--fleet", str(fleet_path)]
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    api = Bridge(proc, port, Path("<fr17-devnull>"))
    url = f"http://{HTTP_HOST}:{port}"
    try:
        api.wait_http_ready()
        fleet_purge(api)   # 清场 (含自动播种的 CLI 兼容桥), 空清单起测

        first = read_prefill(url)
        if first.strip() == "":
            pytest.skip(
                "[BLOCKED-BY-FRONTEND] FR-17 预填未落地 (新建桥弹窗 #npListen "
                "打开时为空, 契约: 预填下一空闲端口); dev-ui 落地即自动放行")
        yield type("Admin", (), {"api": api, "port": port, "url": url,
                                 "read": staticmethod(read_prefill),
                                 "first_prefill": first})
    finally:
        try:
            fleet_purge(api)
        except Exception:
            pass
        api.stop()
        kill_all_bridges()   # 双保险: 绝不残留占 COM1 的进程


@pytest.fixture
def clean_fleet(fr17_admin):
    """每条测试前后清清单, 保证预填判定的确定性。"""
    fleet_purge(fr17_admin.api)
    yield fr17_admin
    fleet_purge(fr17_admin.api)


def make_bridges(admin, listen_ports: list[int]) -> dict[int, str]:
    """按给定端口建桥 (COM1), 返回 {listen_port: bridge_id}。"""
    ids = {}
    for i, lp in enumerate(listen_ports):
        code, resp = fleet_create(admin.api, f"qa-fr17-{i}", "COM1", lp)
        assert code in (200, 201) and not (isinstance(resp, dict)
                                           and resp.get("ok") is False), \
            f"建桥 (listen={lp}) -> {code}: {resp!r}"
        ids[lp] = fleet_new_id(admin.api, resp, lp)
    return ids


def delete_bridge(admin, bid) -> None:
    """删桥 (兼容'须先停'前置, 沿 fleet_purge 口径)。"""
    code, body = admin.api.post(f"/api/fleet/{bid}/delete", {})
    if code >= 400 or (isinstance(body, dict) and body.get("ok") is False):
        admin.api.post(f"/api/fleet/{bid}/stop", {})
        code, body = admin.api.post(f"/api/fleet/{bid}/delete", {})
    assert code in (200, 201) and not (isinstance(body, dict)
                                       and body.get("ok") is False), \
        f"删桥 {bid} -> {code}: {body!r}"


def prefill_port(admin) -> int:
    """读一次预填并解析出端口 (接受 host:port / 裸端口两种回显形状)。"""
    raw = admin.read(admin.url)
    assert raw.strip() != "", "预填值为空 (FR-17 违约: 应预填下一空闲端口)"
    try:
        return listen_port_of(raw)
    except (TypeError, ValueError):
        raise AssertionError(f"预填值 {raw!r} 无法解析出端口")


# ------------------------------------------------------------------ FR-17 用例

def test_fr17a_empty_fleet_prefills_manager_plus_1(clean_fleet):
    """空清单: 预填 = 管理台端口+1 (首个空闲)。"""
    expected = clean_fleet.port + 1
    got = prefill_port(clean_fleet)
    assert got == expected, (
        f"FR-17 违约: 空清单预填应为 {expected} (管理台+1), 实得 {got}")


def test_fr17b_prefill_skips_used_ports(clean_fleet):
    """建两桥占 P+1 / P+2: 预填向上跳过已用 = P+3。"""
    admin = clean_fleet
    make_bridges(admin, [admin.port + 1, admin.port + 2])
    got = prefill_port(admin)
    assert got == admin.port + 3, (
        f"FR-17 违约: 两桥占 {admin.port + 1}/{admin.port + 2} 后预填应为 "
        f"{admin.port + 3}, 实得 {got}")


def test_fr17c_prefill_refills_hole_after_delete(clean_fleet):
    """建两桥 (P+1 / P+2) 后删中间桥 (P+1): 预填回填空洞 = P+1。"""
    admin = clean_fleet
    ids = make_bridges(admin, [admin.port + 1, admin.port + 2])
    delete_bridge(admin, ids[admin.port + 1])
    wait_port_free(admin.port + 1)   # 数据口确已释放, 预填判定不受异步释放干扰
    rows = fleet_rows(admin.api)
    assert len(rows) == 1, f"删中间桥后应剩 1 桥, 实得 {len(rows)}: {rows!r}"
    got = prefill_port(admin)
    assert got == admin.port + 1, (
        f"FR-17 违约: 删中间桥后预填应回填空洞 {admin.port + 1}, 实得 {got}")
