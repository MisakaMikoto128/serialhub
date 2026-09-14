# -*- coding: utf-8 -*-
"""FR-22 配置导入导出黑盒套件 — Sprint 13 批次 B (ADR-24⑤ B4) / spec FR-22。

覆盖 (预期只来自 spec FR-22 + 任务契约):
1  export: GET /api/fleet/export → 200; body 为合法 JSON, 桥集合与 GET /api/fleet
   现有桥对账 (名称/串口/listen/桥数一致)。
2  import merge: 现有桥原样保留 (id/name/串口/listen 不动) + 新增桥进入列表。
3  import replace: 整表替换 —— 旧桥全消失 (id 定位为 None), 新桥恰在列。
4  非法 schema → 400 (导入前校验); mode 越域 (spec: merge|replace 二选一) → 400;
   失败导入不得改动现有表。

契约假设 (spec 留白, 黑盒宽容处理; 落地后如有出入如实记录, 不改预期凑绿,
详见 docs/team/reports/qa-sprint13-plan.md §3):
- 请求体 {"mode": ..., "json": <fleet 文档>}; json 值先试对象编码, 被拒再试
  JSON 字符串编码 (与 conftest fleet_rows 信封宽容同纪律; 若仅字符串编码被受理
  以 warning 记偏差待裁定);
- fleet 文档信封 裸数组 / {"bridges":[...]} 都认; merge/replace 源数据取自
  export 回显 (天然同形), 新桥条目由现有条目改字段派生;
- 非法 schema 用例只发对象编码 (垃圾文档两种编码下都必须被拒, 断言不受编码影响)。

后端未落地时经 fr22_ready 门控整组跳过 [BLOCKED-BY-BACKEND] (见 conftest)。
运行: 在项目根执行  python -m pytest tests/test_fr22_import_export.py -v
"""
from __future__ import annotations

import copy
import json
import warnings

from conftest import (
    BRIDGE_COM,
    HTTP_HOST,
    PEER_COM,
    fleet_create,
    fleet_new_id,
    fleet_purge,
    fleet_rows,
    listen_port_of,
    post_accepted,
    row_of,
    serial_port_of,
    take_port,
)


def _create_ok(api, name: str, serial_port: str, listen: int) -> str:
    code, resp = fleet_create(api, name, serial_port, listen)
    assert code in (200, 201) and not (isinstance(resp, dict) and resp.get("ok") is False), \
        f"POST /api/fleet -> {code}: {resp!r}"
    return fleet_new_id(api, resp, listen)


def _bridges_of(doc):
    """取 fleet 文档的 bridges 集合 (信封宽容: {"bridges":[...]} / 裸数组)。"""
    if isinstance(doc, dict) and isinstance(doc.get("bridges"), list):
        return doc["bridges"]
    if isinstance(doc, list):
        return doc
    raise AssertionError(f"fleet 文档缺 bridges 集合: {str(doc)[:300]!r}")


def _export(api):
    """GET /api/fleet/export → (原始文档, bridges 列表)。"""
    code, body = api.get("/api/fleet/export")
    assert code == 200, f"GET /api/fleet/export -> {code}: {str(body)[:300]!r}"
    assert isinstance(body, (list, dict)), \
        f"export body 非合法 JSON 结构: {type(body).__name__}"
    bridges = _bridges_of(body)
    assert isinstance(bridges, list), f"export bridges 非列表: {str(body)[:300]!r}"
    return body, bridges


def _with_bridges(doc, bridges):
    """按 export 原信封形状替换 bridges 集合。"""
    if isinstance(doc, dict):
        out = dict(doc)
        out["bridges"] = bridges
        return out
    return list(bridges)


def _derived(template: dict, name: str, serial_port: str, listen: int, bid: str) -> dict:
    """由既有条目派生新桥条目: 仅改 name/id/串口/listen, 其余字段保持模板形状。"""
    e = copy.deepcopy(template)
    e["name"] = name
    if "id" in e:
        e["id"] = bid
    s = e.get("serial")
    if isinstance(s, dict):
        s["port"] = serial_port
    else:
        e["serial"] = serial_port
    e["listen"] = f"{HTTP_HOST}:{listen}"
    return e


def _import(api, mode: str, doc):
    """POST /api/fleet/import; json 值先试对象编码, 被拒再试 JSON 字符串编码。

    仅当字符串编码才受理时以 warning 记契约偏差 (spec 留白, 待架构师裁定)。
    """
    code, body = None, None
    for tag, val in (("object", doc), ("string", json.dumps(doc))):
        code, body = api.post("/api/fleet/import", {"mode": mode, "json": val})
        if code in (200, 201) and not (isinstance(body, dict) and body.get("ok") is False):
            if tag == "string":
                warnings.warn(
                    "FR-22 import json 值: 对象编码被拒, JSON 字符串编码才受理 —— "
                    "契约形状待裁定 (spec 留白)", stacklevel=1)
            return code, body
        if code == 404:
            break   # 端点未实现 (fr22_ready 门控兜底), 不再试第二编码
    return code, body


# ------------------------------------------------------------------ 1 export

def test_fr22_export_contains_existing_bridges(start_fleet, fr22_ready):
    api = start_fleet()
    fleet_purge(api)
    port_a, port_b = take_port(18200), take_port(18201)
    _create_ok(api, "qa-fr22-a", BRIDGE_COM, port_a)
    _create_ok(api, "qa-fr22-b", PEER_COM, port_b)
    doc, bridges = _export(api)
    names = {b.get("name") for b in bridges}
    assert {"qa-fr22-a", "qa-fr22-b"} <= names, f"export 缺现有桥: {names!r}"
    assert len(bridges) == len(fleet_rows(api)), \
        f"export 桥数 {len(bridges)} != GET /api/fleet 行数 {len(fleet_rows(api))}"
    by_name = {b.get("name"): b for b in bridges}
    assert serial_port_of(by_name["qa-fr22-a"]) == BRIDGE_COM, \
        f"export 条目串口与建桥不符: {by_name['qa-fr22-a']!r}"
    assert serial_port_of(by_name["qa-fr22-b"]) == PEER_COM, \
        f"export 条目串口与建桥不符: {by_name['qa-fr22-b']!r}"
    assert listen_port_of(by_name["qa-fr22-a"].get("listen")) == port_a, \
        "export 条目 listen 与建桥不符"


# ------------------------------------------------------------------ 2 merge

def test_fr22_import_merge_keeps_existing_and_adds_new(start_fleet, fr22_ready):
    api = start_fleet()
    fleet_purge(api)
    port_a, port_b = take_port(18200), take_port(18201)
    id_a = _create_ok(api, "qa-fr22-a", BRIDGE_COM, port_a)
    doc, bridges = _export(api)
    entry_b = _derived(bridges[0], "qa-fr22-b", PEER_COM, port_b, "qa-fr22-b-id")
    code, body = _import(api, "merge", _with_bridges(doc, bridges + [entry_b]))
    post_accepted(code, body, "/api/fleet/import (merge)")
    rows = fleet_rows(api)
    names = {r["name"] for r in rows}
    assert names == {"qa-fr22-a", "qa-fr22-b"}, f"merge 后应=旧桥+新桥: {names!r}"
    row_a = row_of(api, id_a)
    assert row_a is not None, "merge 后原桥丢失"
    assert row_a["id"] == id_a, f"merge 改动了原桥 id: {row_a!r}"
    assert serial_port_of(row_a) == BRIDGE_COM \
        and listen_port_of(row_a["listen"]) == port_a, f"merge 改动了原桥配置: {row_a!r}"
    row_b = next(r for r in rows if r["name"] == "qa-fr22-b")
    assert serial_port_of(row_b) == PEER_COM, f"新增桥串口不符: {row_b!r}"


# ------------------------------------------------------------------ 3 replace

def test_fr22_import_replace_swaps_whole_table(start_fleet, fr22_ready):
    api = start_fleet()
    fleet_purge(api)
    port_old1, port_old2, port_new = take_port(18200), take_port(18201), take_port(18202)
    id_old1 = _create_ok(api, "qa-fr22-old1", BRIDGE_COM, port_old1)
    id_old2 = _create_ok(api, "qa-fr22-old2", PEER_COM, port_old2)
    doc, bridges = _export(api)
    entry_new = _derived(bridges[0], "qa-fr22-new", BRIDGE_COM, port_new, "qa-fr22-new-id")
    code, body = _import(api, "replace", _with_bridges(doc, [entry_new]))
    post_accepted(code, body, "/api/fleet/import (replace)")
    rows = fleet_rows(api)
    names = {r["name"] for r in rows}
    assert names == {"qa-fr22-new"}, \
        f"replace 应整表替换 (旧桥消失, 新桥恰在列): {names!r}"
    assert row_of(api, id_old1) is None and row_of(api, id_old2) is None, "旧桥未被移除"
    row_new = rows[0]
    assert serial_port_of(row_new) == BRIDGE_COM \
        and listen_port_of(row_new["listen"]) == port_new, f"新桥配置不符: {row_new!r}"


# ------------------------------------------------------------------ 4 非法 schema

def test_fr22_import_rejects_bad_schema_and_keeps_table(start_fleet, fr22_ready):
    api = start_fleet()
    fleet_purge(api)
    id_a = _create_ok(api, "qa-fr22-a", BRIDGE_COM, take_port(18200))
    bad_cases = [
        ("replace", {"bridges": [{"name": 123}]}),   # 字段类型错
        ("replace", {"bridges": "not-a-list"}),      # 集合形状错
        ("merge", {"unrelated": True}),              # 缺 bridges 集合
    ]
    for mode, doc in bad_cases:
        code, body = api.post("/api/fleet/import", {"mode": mode, "json": doc})
        assert code == 400, \
            f"非法 schema 应 400 ({mode}, {doc}): {code}: {str(body)[:200]!r}"
    code, body = api.post("/api/fleet/import",
                          {"mode": "upsert", "json": {"bridges": []}})
    assert code == 400, \
        f"mode 越域 (spec: merge|replace 二选一) 应 400: {code}: {str(body)[:200]!r}"
    # 失败导入不得改动现有表
    names = {r["name"] for r in fleet_rows(api)}
    assert names == {"qa-fr22-a"}, f"失败的导入改动了现有表: {names!r}"
    assert row_of(api, id_a) is not None
