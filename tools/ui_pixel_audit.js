#!/usr/bin/env node
/* ==========================================================================
 * UI-1 像素审计 (ADR-16②, Sprint 6) — SerialHub 管理台控件尺度实测
 *
 * 断言 (spec UI-1 / ADR-16②, 不许"差不多"):
 *   1. 全站可见 button/input/select 的渲染高度 ∈ {28, 34}px (容差 0.5px) —— 两档令牌
 *      (--ctl-h / --ctl-h-sm, 各主题同值, 不随主题变);
 *   2. 圆角 (四角) = 当前主题 --ctl-radius 期望值 (容差 0.5px) —— Sprint 10 主题感知口径:
 *      每轮审计先读页面 #themeCss 得当前主题, 再 GET /themes/<主题>.css 解析出该主题的
 *      --ctl-radius 作为期望, 实测必须 = 期望 ±0.5 (内置 light/dark/example-oreo = 8px
 *      与旧口径一致; win95 = 0 直角, 不再被 8px 硬断言误杀)。主题 CSS 取不到或缺
 *      --ctl-radius 令牌 → 审计硬失败 (期望值不许猜)。
 *   2b. 修复轮 D2 口径: 组合条带 (容器带 data-strip 标记, 如页签/ASCII-HEX 段控) 的
 *       内部子元素圆角由容器承载 —— 子元素免圆角断言, 但高度断言不豁免;
 *       同时断言每个 data-strip 容器自身四角圆角 = 当前主题期望 ±0.5px (严格性不降)。
 *
 * 做法 (真跑真断言, 不目测):
 *   - 自拉真后端 (target/release/serialhub.exe, 默认 127.0.0.1:8080, 临时 fleet 清单);
 *   - 经控制台 API 建一座桥 (COM1) 并启动, 等 phase=open —— "开着桥";
 *   - Playwright 无头 Chromium 走五轮真实页面态, 各截图一轮:
 *       R1 仪表盘 (开着桥) / R2 新建桥弹窗 / R3 抽屉·设置 / R4 抽屉·串口数据 / R5 抽屉·统计
 *       + R6 设置弹窗 / R7 设置弹窗·深色 (Sprint 7 收口 QA 扩轮: FR-13/14 新控件入审计口径);
 *   - 每轮收集页面全部 button/input/select (可见者): getBoundingClientRect().height
 *     + 四角 computed border-radius; 隐藏/零尺寸/opacity:0 元素记 skipped, 不算违例;
 *   - 输出违例清单表格 (轮次/选择器/实测高度/实测圆角/违约原因); 全过 → PASS。
 *
 * 运行 (项目根):
 *   NODE_PATH="$(npm root -g)" node tools/ui_pixel_audit.js
 *   可选: AUDIT_ADDR=127.0.0.1:8080  AUDIT_SHOTS=1 (只截图不断言)
 *   可选: AUDIT_THEME=<name> 单主题审计 (light/dark/example-oreo/win95...):
 *     落 localStorage(sh_theme) 后重载, 七轮全部在该主题下执行 (R7 不再切深色),
 *     截图带主题前缀, 顺带存管理台全景到 docs/team/reports/qa-sprint10/。
 * 串口纪律: 只用 COM1 (审计桥), 全程禁止碰 COM8。审计结束强杀自家后端进程。
 * ========================================================================== */
"use strict";
const { execFileSync, spawn } = require("child_process");
const fs = require("fs");
const http = require("http");
const net = require("net");
const os = require("os");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const EXE = path.join(ROOT, "target", "release", "serialhub.exe");
const ADDR = process.env.AUDIT_ADDR || "127.0.0.1:8080";
const HOST = ADDR.split(":")[0];
const PORT = Number(ADDR.split(":")[1]);
const BASE = `http://${ADDR}`;
const SHOTS_DIR = path.join(ROOT, "output", "playwright");
const ONLY_SHOTS = !!process.env.AUDIT_SHOTS;

const ALLOWED_H = [28, 34];
const TOL_H = 0.5;
const TOL_R = 0.5;
/* Sprint 10 主题感知: AUDIT_THEME 非空 = 单主题审计 (全轮锁定该主题);
 * 空 = 沿用旧流程 (默认浅色起跑, R7 切深色复测)。圆角期望一律从主题 CSS 实时解析。 */
const AUDIT_THEME = process.env.AUDIT_THEME || "";

// ---- playwright 解析: 优先 NODE_PATH, 退回 Windows npm 全局根 ----------------
function loadPlaywright() {
  try { return require("playwright"); }
  catch (_) { /* fallthrough */ }
  const guess = path.join(process.env.APPDATA || "", "npm", "node_modules", "playwright");
  try { return require(guess); }
  catch (_) {
    console.error("无法加载 playwright —— 请运行: NODE_PATH=\"$(npm root -g)\" node tools/ui_pixel_audit.js");
    process.exit(2);
  }
}

function httpGet(url, timeoutMs = 3000) {
  return new Promise((resolve) => {
    const req = http.get(url, { timeout: timeoutMs }, (res) => {
      let raw = "";
      res.on("data", (c) => (raw += c));
      res.on("end", () => resolve({ code: res.statusCode, body: raw }));
    });
    req.on("timeout", () => { req.destroy(); resolve({ code: 0, body: "timeout" }); });
    req.on("error", (e) => resolve({ code: 0, body: String(e) }));
  });
}

async function waitHttp(url, timeoutMs, what) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    const r = await httpGet(url, 1500);
    if (r.code === 200) return r;
    await new Promise((s) => setTimeout(s, 150));
  }
  throw new Error(`${timeoutMs}ms 内未就绪: ${what} (${url})`);
}

// ---- Sprint 10 主题感知: 圆角期望值从主题 CSS 实时解析 (缓存/主题) -----------
// 页面当前主题名: #themeCss 的 href (/themes/<name>.css); link 未挂 = 内置浅色 light。
const THEME_JS = () => {
  const l = document.getElementById("themeCss");
  const m = l && (l.getAttribute("href") || "").match(/\/themes\/([^/?#]+)\.css/);
  return m ? decodeURIComponent(m[1]) : "light";
};

// GET /themes/<主题>.css 解析 --ctl-radius (px 可省, 如 win95 直角写 0);
// CSS 取不到或缺令牌 → 抛错硬失败 (期望值不许猜, 不许"差不多")。
const radiusCache = new Map();
async function themeRadius(theme) {
  if (radiusCache.has(theme)) return radiusCache.get(theme);
  const r = await httpGet(`${BASE}/themes/${encodeURIComponent(theme)}.css`);
  if (r.code !== 200) {
    throw new Error(`主题 CSS 取不到: /themes/${theme}.css -> HTTP ${r.code} (后端未内置该主题?)`);
  }
  const m = r.body.match(/--ctl-radius\s*:\s*([0-9]+(?:\.[0-9]+)?)(?:px)?/);
  if (!m) throw new Error(`主题 ${theme}.css 缺 --ctl-radius 令牌, 圆角期望值无法确定`);
  const v = Number(m[1]);
  radiusCache.set(theme, v);
  console.log(`  主题 ${theme}: --ctl-radius 期望 ${v}px`);
  return v;
}

function killSelfSweep() {
  try { execFileSync("taskkill", ["/F", "/IM", "serialhub.exe", "/T"], { stdio: "ignore" }); }
  catch (_) { /* 无残留即报错, 忽略 */ }
}

async function freeTcpPort(preferred) {
  const tryBind = (p) => new Promise((res) => {
    const s = net.createServer();
    s.once("error", () => res(false));
    s.once("listening", () => s.close(() => res(true)));
    s.listen(p, HOST);
  });
  if (preferred && (await tryBind(preferred))) return preferred;
  return new Promise((res) => {
    const s = net.createServer();
    s.listen(0, HOST, () => { const p = s.address().port; s.close(() => res(p)); });
  });
}

// ---- 页面态采集: 全部可见 button/input/select 的高度 + 圆角 ------------------
const COLLECT_JS = () => {
  const desel = (el) => {
    const tag = el.tagName.toLowerCase();
    const id = el.id ? `#${el.id}` : "";
    const cls = (el.classList && el.classList.length) ? "." + [...el.classList].slice(0, 2).join(".") : "";
    let txt = "";
    if (tag === "button") txt = ` "${(el.textContent || "").trim().slice(0, 14)}"`;
    const ph = el.getAttribute && el.getAttribute("placeholder");
    const extra = ph ? ` [ph=${ph.slice(0, 12)}]` : "";
    return `${tag}${id}${cls}${txt}${extra}`;
  };
  const inDialog = (el) => !!el.closest("dialog[open]");
  const out = [];
  for (const el of document.querySelectorAll("button, input, select")) {
    const cs = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    const visible = !(r.width === 0 && r.height === 0) && cs.display !== "none"
      && cs.visibility !== "hidden" && Number(cs.opacity) > 0.01
      && !el.closest("[hidden]");
    if (!visible) continue;
    const radii = [cs.borderTopLeftRadius, cs.borderTopRightRadius,
                   cs.borderBottomRightRadius, cs.borderBottomLeftRadius]
      .map((v) => parseFloat(v) || 0);
    out.push({
      selector: desel(el),
      where: inDialog(el) ? "dialog" : (el.closest("#drawer") ? "drawer" : "page"),
      height: Math.round(r.height * 100) / 100,
      radius: radii,
      type: el.getAttribute("type") || "",
      strip: !!(el.closest && el.closest("[data-strip]")),   // D2: 组合条带内子元素 (圆角由容器承载)
    });
  }
  return out;
};
// D2: data-strip 容器自身仍须承载圆角 (断言上移到容器, 期望值随当前主题 --ctl-radius, 严格性不降)
const STRIP_JS = () => {
  const out = [];
  for (const el of document.querySelectorAll("[data-strip]")) {
    const cs = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) continue;
    out.push({
      selector: `${el.tagName.toLowerCase()}${el.id ? "#" + el.id : ""}[data-strip]`,
      radius: [cs.borderTopLeftRadius, cs.borderTopRightRadius,
               cs.borderBottomRightRadius, cs.borderBottomLeftRadius].map((v) => parseFloat(v) || 0),
    });
  }
  return out;
};

(async () => {
  if (!fs.existsSync(EXE)) {
    console.error(`未找到 ${EXE} —— 请先在项目根执行 cargo build --release`);
    process.exit(2);
  }
  fs.mkdirSync(SHOTS_DIR, { recursive: true });
  killSelfSweep();

  // ---- 拉起真后端 (临时 fleet, 不污染全局配置) ----
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "serialhub_ui_audit_"));
  const fleetPath = path.join(tmp, "fleet.json");
  const logPath = path.join(tmp, "backend.log");
  const backend = spawn(EXE, ["--headless", "--addr", ADDR, "--fleet", fleetPath],
    { stdio: ["ignore", fs.openSync(logPath, "w"), "inherit"] });
  let backendDead = false;
  backend.on("exit", () => { backendDead = true; });
  const cleanup = () => {
    try { if (!backendDead) execFileSync("taskkill", ["/F", "/PID", String(backend.pid), "/T"], { stdio: "ignore" }); }
    catch (_) {}
    killSelfSweep();
  };
  process.on("exit", cleanup);
  process.on("SIGINT", () => process.exit(130));

  try {
    await waitHttp(`${BASE}/api/fleet`, 20000, "后端控制面 /api/fleet");

    const { chromium } = loadPlaywright();
    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    await page.goto(BASE, { waitUntil: "networkidle", timeout: 20000 });
    await page.waitForSelector("#btnNew", { timeout: 10000 });

    // ---- 经页面 fetch 建桥并启动 (COM1), 等 phase=open —— "开着桥" ----
    const listenPort = await freeTcpPort(8101);
    const created = await page.evaluate(async ([lp]) => {
      const mk = (m, u, b) => fetch(u, { method: m, headers: { "Content-Type": "application/json" }, body: b ? JSON.stringify(b) : undefined });
      let r = await mk("POST", "/api/fleet", {
        name: "qa-ui-audit",
        serial: { port: "COM1", baud: 115200, dataBits: 8, parity: "N", stopBits: 1, flow: "none" },
        listen: `127.0.0.1:${lp}`,
      });
      if (!r.ok) return { ok: false, step: "create", code: r.status, body: await r.text() };
      const j = await r.json().catch(() => ({}));
      r = await mk("POST", `/api/fleet/${encodeURIComponent(j.id)}/start`, {});
      return { ok: r.ok, step: "start", code: r.status, body: r.ok ? "" : await r.text() };
    }, [listenPort]);
    if (!created.ok) throw new Error(`建桥/启动失败 (${created.step} -> ${created.code}): ${created.body}`);

    let opened = false;
    for (let i = 0; i < 100; i++) {                       // 等 phase=open (≤20s)
      const rows = await page.evaluate(async () => {
        const r = await fetch("/api/fleet"); return r.json();
      });
      const list = Array.isArray(rows) ? rows : (rows.bridges || []);
      const row = list.find((b) => b && b.name === "qa-ui-audit");
      if (row && row.phase === "open") { opened = true; break; }
      if (row && row.phase === "retry") break;            // open 必败 (COM1 被占) → 不算"开着桥"
      await page.waitForTimeout(200);
    }
    if (!opened) throw new Error("审计桥未达 open —— 请确认 COM1 空闲 (审计要求\"开着桥\")");

    const violations = [];
    const rounds = [];

    async function audit(round, shot) {
      await page.waitForTimeout(250);                     // 布局/动画落定
      const theme = await page.evaluate(THEME_JS);        // Sprint 10: 本轮生效主题
      const wantR = await themeRadius(theme);             // 该主题 --ctl-radius 期望值
      const file = path.join(SHOTS_DIR, shot);
      await page.screenshot({ path: file, fullPage: false });
      const ctrls = await page.evaluate(COLLECT_JS);
      let bad = 0;
      for (const c of ctrls) {
        const why = [];
        const hOk = ALLOWED_H.some((h) => Math.abs(c.height - h) <= TOL_H);
        if (!hOk) why.push(`高度 ${c.height}px ∉ {28,34}±0.5`);
        if (!c.strip) {                                   // D2: 条带内子元素圆角由容器承载, 免断言 (高度不豁免)
          const rBad = c.radius.filter((v) => Math.abs(v - wantR) > TOL_R);
          if (rBad.length) why.push(`圆角 [${c.radius.join("/")}]px ≠ ${wantR}±0.5 (主题 ${theme})`);
        }
        if (why.length) { bad++; violations.push({ round, ...c, reason: why.join("; ") }); }
      }
      const strips = await page.evaluate(STRIP_JS);
      let stripBad = 0;
      for (const s of strips) {
        const rBad = s.radius.filter((v) => Math.abs(v - wantR) > TOL_R);
        if (rBad.length) {
          stripBad++; bad++;
          violations.push({ round, selector: s.selector, where: "strip-container",
            height: "", radius: s.radius, reason: `条带容器圆角 [${s.radius.join("/")}]px ≠ ${wantR}±0.5 (主题 ${theme})` });
        }
      }
      rounds.push({ round, shot, theme, wantR, controls: ctrls.length, strips: strips.length, violations: bad });
      console.log(`  ${round} [${theme}, 期望圆角 ${wantR}px]: 截图 ${path.relative(ROOT, file)} | 可见控件 ${ctrls.length} + 条带容器 ${strips.length} | 违例 ${bad}`);
    }

    // Sprint 10 AUDIT_THEME: 单主题审计 —— 落 localStorage 后重载, 七轮全部锁定该主题
    // (页面 <head> 预挂逻辑会按 sh_theme 挂 #themeCss; 桥在后端, 重载不丢 "开着桥" 态)
    const TAG = AUDIT_THEME ? `ui-audit-${AUDIT_THEME}-` : "ui-audit-";
    if (AUDIT_THEME) {
      await page.evaluate((t) => { try { localStorage.setItem("sh_theme", t); } catch (_) {} }, AUDIT_THEME);
      await page.reload({ waitUntil: "networkidle", timeout: 20000 });
      await page.waitForSelector("#btnNew", { timeout: 10000 });
      await page.waitForFunction((t) => {
        const l = document.getElementById("themeCss");
        return !!l && (l.getAttribute("href") || "").includes("/themes/" + t + ".css");
      }, AUDIT_THEME, { timeout: 5000 });
      await page.waitForTimeout(400);                     // 换肤渲染落定
      console.log(`单主题审计: 全轮锁定 ${AUDIT_THEME}`);
    }

    console.log(`审计目标: ${BASE} (桥 qa-ui-audit@COM1 open, listen ${listenPort})`);
    // R1 仪表盘 (开着桥)
    await audit("R1-仪表盘", TAG + "1-dashboard.png");
    if (AUDIT_THEME) {
      // 单主题审计顺带存管理台全景 (fullPage) 到 qa 报告目录
      const panoDir = path.join(ROOT, "docs", "team", "reports", "qa-sprint10");
      fs.mkdirSync(panoDir, { recursive: true });
      const pano = path.join(panoDir, `admin-panorama-${AUDIT_THEME}.png`);
      await page.screenshot({ path: pano, fullPage: true });
      console.log(`  管理台全景 (${AUDIT_THEME}): ${path.relative(ROOT, pano)}`);
    }
    // R2 新建桥弹窗
    await page.click("#btnNew");
    await page.waitForSelector("#dlgNew[open]", { timeout: 5000 });
    await audit("R2-新建桥弹窗", TAG + "2-new-dialog.png");
    await page.click("#btnDlgX");
    await page.waitForSelector("#dlgNew[open]", { state: "detached" }).catch(() => {});
    // R3-R5 抽屉三页签
    await page.locator(".bcard", { hasText: "qa-ui-audit" }).locator(".act-cfg").first().click();
    await page.waitForSelector("#drawer:not([hidden])", { timeout: 5000 });
    await audit("R3-抽屉·设置", TAG + "3-drawer-cfg.png");
    await page.click("#tb-tap");
    await audit("R4-抽屉·串口数据", TAG + "4-drawer-tap.png");
    await page.click("#tb-stats");
    await audit("R5-抽屉·统计", TAG + "5-drawer-stats.png");

    // R6 设置弹窗 (Sprint 7 FR-13/14, QA 收口扩轮): 主题选择器 + 管理台网址 +
    //   应用/取消/关闭钮; 页头「打开面板」「设置」钮在每轮全量收集里已覆盖。
    //   先 reload 隔离 R5 的抽屉态, 保证本轮所见即"纯设置弹窗"。
    await page.reload();
    await page.waitForTimeout(500);
    await page.click("#btnSettings");
    await page.waitForSelector("#dlgSettings[open]", { timeout: 5000 });
    await audit("R6-设置弹窗", TAG + "6-settings.png");
    if (AUDIT_THEME) {
      // 单主题审计: R7 留在本主题 (不再切深色), 与其余各轮同口径
      await audit(`R7-设置弹窗·${AUDIT_THEME}`, TAG + "7-settings.png");
      await page.click("#btnSetX");
    } else {
      // R7 设置弹窗·深色 (FR-14 无刷新换肤后同口径复测; 兼作深色主题冒烟截图)
      await page.selectOption("#setTheme", "dark");
      await page.waitForFunction(() => {
        const l = document.getElementById("themeCss");
        return !!l && (l.href || "").includes("/themes/dark.css");
      }, { timeout: 5000 });
      await page.waitForTimeout(300);                     // 换肤渲染落定
      await audit("R7-设置弹窗·深色", "ui-audit-7-settings-dark.png");
      await page.click("#btnSetX");
    }

    await browser.close();

    // ---- 结果 ----
    const total = rounds.reduce((a, r) => a + r.controls, 0);
    console.log("\n===== UI-1 像素审计结论 =====");
    for (const r of rounds) console.log(`  ${r.round} [${r.theme} → 圆角期望 ${r.wantR}px]: 控件 ${r.controls}, 违例 ${r.violations}`);
    if (violations.length === 0) {
      const stripTotal = rounds.reduce((a, r) => a + r.strips, 0);
      console.log(`PASS — ${total} 个可见控件 × ${rounds.length} 轮全部满足: 高度 ∈ {28,34}±0.5px, 圆角 = 当前主题 --ctl-radius ±0.5px (Sprint 10 主题感知口径; 组合条带子元素圆角由容器承载, ${stripTotal} 个条带容器圆角实测 = 主题期望)`);
      process.exit(0);
    }
    console.log(`FAIL — ${violations.length}/${total} 违例 (UI-1/ADR-16②):`);
    console.table(violations.map((v) => ({
      轮次: v.round, 区域: v.where, 元素: v.selector,
      高度px: v.height, 圆角px: v.radius.join("/"), 违约: v.reason,
    })));
    process.exit(1);
  } catch (e) {
    console.error("审计中止:", e.message);
    console.error(`后端日志: ${logPath}`);
    process.exit(2);
  }
})();
