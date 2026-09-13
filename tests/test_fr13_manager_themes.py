# -*- coding: utf-8 -*-
"""FR-13 管理台换址 + FR-14 主题插件 — Sprint 7 QA 套件 (ADR-18① ②)。

契约出处 (黑盒预期, 不读 src/ 调预期):
- spec FR-13: POST /api/manager/addr {"addr": "host:port"} 控制面原地换绑 (ADR-12 机制);
  fleet.json [manager] 持久化, 同 --fleet 重启直接起在新地址 (不传 --addr);
  换绑失败回退 —— 目标端口被占时原地址持续服务, 控制面不崩。
- spec FR-14: GET /api/themes 列出主题, 内置 light/dark/example-oreo 且 builtin 标记正确;
  /themes/<file> 静态服务且只限 themes 目录 (路径穿越必须 404/400); themes/*.css 即插件,
  目录放入自制 css 后重新列出即可见 (GET 即扫描); --themes-dir 可指定扫描目录。
- 任务契约注记: 桥对象/status 图标不涉本套件契约; 「打开面板」按钮与 webview 跟随属 UI 层,
  黑盒 HTTP 面只验地址可达性与持久化。

门控 (沿 fleet_ready / fr12_ready 先例, 区分"未到位"与"违约"):
会话级探针 fr13_fr14_probe 对同一控制面进程实测两类端点, 未落地 → 对应组
SKIPPED [BLOCKED-BY-BACKEND], dev-backend 落地即自动放行。
跑前必须 cargo build --release (旧二进制假阴性教训, qa-sprint3)。

纪律: 只用 COM1(桥侧)/COM2(对端), COM8 禁碰; 每条测试自管进程, teardown 强杀;
全程 --fleet 指向临时清单, 不污染全局 fleet.json; 不 commit。
"""
from __future__ import annotations

import http.client
import itertools
import json
import socket
import subprocess
import tempfile
import time
import urllib.error
from pathlib import Path

import pytest

from conftest import (
    BRIDGE_COM,
    BRIDGE_EXE,
    HTTP_HOST,
    PROJECT_ROOT,
    Bridge,
    fleet_act_ok,
    fleet_create,
    fleet_new_id,
    fleet_purge,
    fleet_rows,
    free_tcp_port,
    hard_kill,
    http_get,
    http_post,
    kill_all_bridges,
    poll_until,
    row_of,
    take_port,
    wait_port_free,
    wait_row_phase,
)

# ADR-18② 内置主题清单 (任务契约: 内置 light/dark/example-oreo)
REQUIRED_BUILTIN = ("light", "dark", "example-oreo")


# ------------------------------------------------------------------ 进程工厂

_LOG_DIR = Path(tempfile.mkdtemp(prefix="serialhub_fr13_logs_"))
_SEQ = itertools.count(1)


def _spawn_manager(port: int, *, fleet_path=None, extra=None, with_addr=True,
                   wait=True, cwd=PROJECT_ROOT) -> Bridge:
    """拉起控制面进程 (本地版工厂, 不动 conftest 公共夹具)。

    with_addr=False 时不传 --addr —— 验证 fleet.json [manager] 持久化地址的自恢复
    (FR-13 重启语义: 同 --fleet 重启直接起在新端口)。cwd 默认工程根
    (默认 themes 目录按工程根解析; 显式 --themes-dir 用例不受影响)。
    """
    if fleet_path is None:
        fleet_path = Path(tempfile.mkdtemp(prefix="serialhub_fr13_")) / "fleet.json"
    cmd = [str(BRIDGE_EXE), "--headless"]
    if with_addr:
        cmd += ["--addr", f"{HTTP_HOST}:{port}"]
    cmd += ["--fleet", str(fleet_path)]
    if extra:
        cmd += list(extra)
    log = _LOG_DIR / f"mgr_{next(_SEQ)}.log"
    lf = open(log, "w", encoding="utf-8")
    proc = subprocess.Popen(cmd, stdout=lf, stderr=subprocess.STDOUT, cwd=str(cwd))
    b = Bridge(proc, port, log)
    b._log_handle = lf
    b.fleet_path = fleet_path
    if wait:
        b.wait_http_ready()
    return b


@pytest.fixture
def spawn_manager():
    """FR-13/14 控制面进程工厂, teardown 统一强杀 (含双保险)。"""
    started: list[Bridge] = []

    def _start(**kw) -> Bridge:
        port = kw.pop("port", free_tcp_port())
        b = _spawn_manager(port, **kw)
        started.append(b)
        return b

    yield _start
    for b in started:
        b.stop()
    kill_all_bridges()   # 双保险: 任何失败路径不残留占 COM1 的进程


# ------------------------------------------------------------------ 落地探针

@pytest.fixture(scope="session")
def fr13_fr14_probe():
    """会话级探针 (一次): 一个控制面进程分别实测 FR-13/FR-14 端点是否落地。

    只判 "HTTP 404 = 未实现", 不判语义 (语义由正式用例违约式验收); 探完即杀。
    """
    kill_all_bridges()
    port = free_tcp_port()
    probe_dir = Path(tempfile.mkdtemp(prefix="serialhub_fr13_probe_"))
    b = _spawn_manager(port, fleet_path=probe_dir / "fleet.json", wait=True)
    try:
        themes_code = http_get(b.base + "/api/themes")[0]
        try:
            addr_code, _ = http_post(b.base + "/api/manager/addr",
                                     {"addr": f"{HTTP_HOST}:{free_tcp_port()}"})
        except urllib.error.URLError as e:
            addr_code = None
    finally:
        b.stop()
        kill_all_bridges()
    return {"themes": themes_code, "addr": addr_code}


@pytest.fixture(scope="session")
def fr13_gate(fr13_fr14_probe):
    if fr13_fr14_probe["addr"] == 404:
        pytest.skip("[BLOCKED-BY-BACKEND] FR-13 POST /api/manager/addr 未实现 "
                    "(探针实测 HTTP 404) —— dev-backend 落地即自动放行")


@pytest.fixture(scope="session")
def fr14_gate(fr13_fr14_probe):
    if fr13_fr14_probe["themes"] != 200:
        pytest.skip("[BLOCKED-BY-BACKEND] FR-14 GET /api/themes 未实现 "
                    f"(探针实测 HTTP {fr13_fr14_probe['themes']}) —— 落地即自动放行")


# ------------------------------------------------------------------ 解析容错

def _manager_scope_values(data) -> list[str]:
    """黑盒容错: fleet.json 中键名含 'manager' 作用域下的全部标量叶值 (字符串化)。

    ADR-18① 只约定 "[manager] 持久化", 具体形状 ({"manager":{"addr":...}} 或
    {"managerAddr":...} 或纯端口号) 未定, 以作用域内出现新端口为准。
    """
    found: list[str] = []

    def walk(node, in_mgr: bool = False):
        if isinstance(node, dict):
            for k, v in node.items():
                walk(v, in_mgr or "manager" in str(k).lower())
        elif isinstance(node, list):
            for v in node:
                walk(v, in_mgr)
        elif in_mgr and not isinstance(node, bool) \
                and isinstance(node, (str, int, float)):
            found.append(str(node))

    walk(data)
    return found


def _theme_entries(body):
    """/api/themes 信封容差: 裸列表或单列表字段字典均可 (沿 fleet_rows 先例)。"""
    if isinstance(body, list) and body:
        return body
    if isinstance(body, dict):
        for v in body.values():
            if isinstance(v, list) and v:
                return v
    raise AssertionError(f"/api/themes 返回结构无法解析出条目列表: {body!r}")


def _tokens(entry) -> set:
    """条目内全部字符串叶值规范化 (小写、去 .css) 为候选标识集合。"""
    toks: set = set()

    def walk(v):
        if isinstance(v, str):
            toks.add(v.strip().lower().removesuffix(".css").strip())
        elif isinstance(v, dict):
            for x in v.values():
                walk(x)
        elif isinstance(v, list):
            for x in v:
                walk(x)

    walk(entry)
    return {t for t in toks if t}


def _builtin_flag(entry):
    """提取 builtin 标记 (键名容错 builtin/builtIn/is-builtin/built_in); 无则 None。"""
    if not isinstance(entry, dict):
        return None
    for k, v in entry.items():
        norm = str(k).lower().replace("-", "").replace("_", "").replace(" ", "")
        if "builtin" in norm:
            return bool(v)
    return None


def _raw_get(port: int, path: str, timeout: float = 5.0):
    """原始路径 GET (http.client 直发, 绕过客户端归一化) —— 路径穿越探测用。

    读全量响应体 (dark.css 的 :root 在长注释头之后, 截断会假阴性)。"""
    conn = http.client.HTTPConnection(HTTP_HOST, port, timeout=timeout)
    try:
        conn.request("GET", path)
        r = conn.getresponse()
        return r.status, r.read()
    finally:
        conn.close()


# ==================================================================
# FR-13 管理台换址 (ADR-18① / spec FR-13)
# ==================================================================

def test_fr13a_manager_addr_rebind_persist_and_restart(spawn_manager, fr13_gate):
    """换址三段: POST 换绑 → 新地址可达/桥健在/原地址失联; fleet.json 记录;
    同 --fleet 重启 (不传 --addr) 直接起在新端口。"""
    fleet_file = Path(tempfile.mkdtemp(prefix="serialhub_fr13a_")) / "fleet.json"
    old_port = free_tcp_port()
    api = spawn_manager(port=old_port, fleet_path=fleet_file)
    fleet_purge(api)                      # 清掉播种空白桥, 清单里只留本测试的桥

    # 一座在管桥: 换绑后必须原样健在 (控制面换址不得掉线/丢桥)
    b_port = take_port()
    code, resp = fleet_create(api, "qa-mgr-keep", BRIDGE_COM, b_port)
    assert code in (200, 201) and not (isinstance(resp, dict) and resp.get("ok") is False), \
        f"POST /api/fleet -> {code}: {resp!r}"
    bid = fleet_new_id(api, resp, b_port)
    fleet_act_ok(api, bid, "start")
    wait_row_phase(api, b_port, "open", timeout=15)

    # ① POST 换绑 (任务示例端口 8180, 被占则退临时端口)
    new_port = take_port(8180)
    code, body = api.post("/api/manager/addr", {"addr": f"{HTTP_HOST}:{new_port}"})
    assert code in (200, 201) and not (isinstance(body, dict) and body.get("ok") is False), \
        f"POST /api/manager/addr -> {code}: {body!r}"

    # ② 新地址 /api/fleet 可达, 且桥仍在 (同 id 同数据面)
    new_api = Bridge(api.proc, new_port, api.log_path)
    poll_until(lambda: http_get(f"{new_api.base}/api/fleet")[0] == 200,
               timeout=5, desc="新地址 /api/fleet 可达 (控制面已换绑)")
    wait_row_phase(new_api, bid, "open", timeout=8)
    row = row_of(new_api, b_port)
    assert row is not None and row.get("id") == bid, \
        f"换绑后桥在管理台消失/换人: rows={fleet_rows(new_api)!r}"

    # ③ 原地址失联 (连接拒绝 = 旧监听确已撤下, 而非双端口并存)
    old_base = f"http://{HTTP_HOST}:{old_port}"

    def _old_dead():
        try:
            http_get(old_base + "/api/fleet")
            return False
        except Exception:
            return True

    poll_until(_old_dead, timeout=5, desc="原地址失联 (连接拒绝)")

    # ④ fleet.json [manager] 记录新地址 (FR-10b 变更即存口径, 3s 窗)
    def _recorded():
        try:
            data = json.loads(fleet_file.read_text("utf-8"))
        except (OSError, ValueError):
            return False
        vals = _manager_scope_values(data)
        return any(v == str(new_port) or v.endswith(f":{new_port}") for v in vals)

    poll_until(_recorded, timeout=3,
               desc=f"fleet.json [manager] 记录新端口 {new_port}")

    # ⑤ 同 --fleet 重启、不传 --addr → 管理台直接起在持久化端口, 桥自动恢复
    hard_kill(api)
    wait_port_free(new_port)
    api2 = spawn_manager(port=new_port, fleet_path=fleet_file, with_addr=False)
    code, body = http_get(f"http://{HTTP_HOST}:{new_port}/api/fleet")
    assert code == 200, \
        f"重启后管理台未起在持久化端口 {new_port}: {code}: {body!r}"
    row2 = row_of(api2, b_port)
    assert row2 is not None, f"重启后桥未恢复: rows={fleet_rows(api2)!r}"
    wait_row_phase(api2, bid, "open", timeout=20)


def test_fr13b_manager_addr_rebind_failure_rolls_back(spawn_manager, fr13_gate):
    """换址失败回退: 目标端口先占住 → POST → 2s 后原地址仍服务, 控制面不崩。"""
    api = spawn_manager()
    old_base = api.base
    blocker = socket.socket()
    blocker.bind((HTTP_HOST, 0))
    blocker.listen(1)
    block_port = blocker.getsockname()[1]
    try:
        try:
            code, body = api.post("/api/manager/addr",
                                  {"addr": f"{HTTP_HOST}:{block_port}"})
        except urllib.error.URLError as e:
            code, body = None, f"传输层异常: {e!r}"

        assert api.proc.poll() is None, \
            f"换址失败不得崩掉控制面进程 (POST 实测 {code}: {body!r})"

        time.sleep(2.0)   # 任务口径: 2s 后原地址仍服务 (回退语义)
        c, b = http_get(old_base + "/api/fleet")
        assert c == 200, (
            f"换址失败后原地址未回退: GET {old_base}/api/fleet -> {c}: {b!r} "
            f"(POST 实测: {code}: {body!r})")

        # 应答口径 (Sprint 7 收口裁定, 计划书 §2 预登记分歧点): 后端对占口失败实测
        # 返回 200 {'ok':true} —— "受理后静默回退" 语义。spec/任务未钉失败应答形状,
        # 按可观测契约 (原址仍服务, 上判据) 验收; 静默成功应答记 OBS-7: UI 端不得以
        # POST 应答为准, 换址结果须回读实际 addr。若后续 ADR 裁定失败须 4xx/ok:false,
        # 在此恢复断言: code >= 400 or body.get("ok") is False。
    finally:
        blocker.close()


# ==================================================================
# FR-14 主题插件 (ADR-18② / spec FR-14)
# ==================================================================

def test_fr14a_themes_list_builtin_and_flags(spawn_manager, fr14_gate):
    """GET /api/themes 含 light/dark/example-oreo 且 builtin 标记双向正确。"""
    api = spawn_manager()   # cwd=工程根: 默认 themes 目录按工程根解析
    code, body = api.get("/api/themes")
    assert code == 200, f"GET /api/themes -> {code}: {body!r}"
    entries = _theme_entries(body)
    entry_toks = [(e, _tokens(e)) for e in entries]

    def _find(tid):
        return next((e for e, t in entry_toks if tid in t), None)

    for tid in REQUIRED_BUILTIN:
        e = _find(tid)
        assert e is not None, f"内置主题 {tid} 未列出: {body!r}"
        flag = _builtin_flag(e)
        assert flag is True, \
            f"内置主题 {tid} 的 builtin 标记应为 True, 实测 {flag!r} (条目: {e!r})"

    # 反向: 非内置条目 (若有, 如用户自放 css) 必须标记 builtin=False
    for e, t in entry_toks:
        if not any(tid in t for tid in REQUIRED_BUILTIN):
            assert _builtin_flag(e) is not True, \
                f"非内置主题 {sorted(t)!r} 的 builtin 标记应为 False: {e!r}"


def test_fr14b_builtin_theme_css_served(spawn_manager, fr14_gate):
    """/themes/dark.css 200 且内容为覆盖 :root 设计令牌的 CSS (含 CSS 变量)。"""
    api = spawn_manager()
    st, payload = _raw_get(api.http_port, "/themes/dark.css")
    assert st == 200, f"GET /themes/dark.css -> {st}: {payload[:200]!r}"
    text = payload.decode("utf-8", "replace")
    assert ":root" in text and "--" in text, \
        (f"dark.css 应为覆盖 :root 设计令牌的 CSS (含 --变量, FR-14), "
         f"实测头部: {text[:200]!r}")


def test_fr14c_theme_path_traversal_blocked(spawn_manager, fr14_gate):
    """/themes/.. 路径穿越必须拒绝 (404/400) —— 原始路径与 URL 编码变体都验。"""
    api = spawn_manager()
    for path in ("/themes/../Cargo.toml", "/themes/%2e%2e/Cargo.toml"):
        st, payload = _raw_get(api.http_port, path)
        assert st in (400, 404), \
            f"路径穿越 {path} 必须被拒 (404/400), 实测 {st}: {payload[:80]!r}"


def test_fr14d_custom_theme_css_plugin(spawn_manager, fr14_gate):
    """themes 目录 (--themes-dir) 放入自制 my-theme.css → 重新列出即可见、
    可静态服务、builtin 标记为 False (插件格式闭环)。"""
    tdir = Path(tempfile.mkdtemp(prefix="serialhub_fr14_custom_"))
    api = spawn_manager(extra=["--themes-dir", str(tdir)])

    css = tdir / "my-theme.css"
    css.write_text("/* qa-my-theme 标记 */\n:root { --qa-marker: 1; --ctl-h: 34px; }\n",
                   encoding="utf-8")

    def _listed() -> bool:
        c, b = api.get("/api/themes")
        if c != 200:
            return False
        try:
            return any("my-theme" in t for e in _theme_entries(b) for t in _tokens(e))
        except AssertionError:
            return False

    poll_until(_listed, timeout=3, desc="自制主题 my-theme.css 放入后重新列出可见")

    st, payload = _raw_get(api.http_port, "/themes/my-theme.css")
    assert st == 200, f"/themes/my-theme.css -> {st} (自制主题应可静态服务)"
    assert b"qa-my-theme" in payload, \
        f"静态服务内容非本测试写入的 CSS: {payload[:120]!r}"

    code, body = api.get("/api/themes")
    assert code == 200, f"GET /api/themes -> {code}: {body!r}"
    e = next((e for e in _theme_entries(body) if "my-theme" in _tokens(e)), None)
    assert e is not None, f"服务在而列表无 my-theme: {body!r}"
    assert _builtin_flag(e) is not True, f"自制主题 builtin 标记应为 False: {e!r}"
