#!/usr/bin/env node
/* ==========================================================================
 * FR-17 预填探针 (Sprint 8 QA, ADR-19②) — 黑盒只读, 不提交表单
 *
 * 用法:  NODE_PATH="$(npm root -g)" node tests/playwright_prefill.cjs <管理台URL>
 * 输出:  末行 JSON  {ok:true, prefill:"127.0.0.1:8091"}  /  {ok:false, error:"..."}
 *        prefill = 新建桥弹窗「网址端口」输入框 (#npListen) 打开时的预填值 (原样)。
 *
 * 做法 (沿 tools/ui_pixel_audit.js 先例): Playwright 无头 Chromium 打开真后端管理台,
 * 点「＋新建桥」(#btnNew 或空态 #btnNew2) → 等 <dialog id=dlgNew> 打开 → 读 #npListen
 * 的 value (最长等 5s, 兼容异步预填) → 点 ✕ 关闭 → 退出。不点「创建并启动」。
 * ========================================================================== */
"use strict";
const { chromium } = require("playwright");

(async () => {
  const url = process.argv[2];
  if (!url) throw new Error("usage: node playwright_prefill.cjs <url>");

  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 20000 });

    // 空态显示 #btnNew2, 有桥显示 #btnNew —— 谁可见点谁
    const opener = page.locator("#btnNew:visible, #btnNew2:visible").first();
    await opener.waitFor({ state: "visible", timeout: 15000 });
    await opener.click();

    await page.waitForSelector("#dlgNew[open]", { timeout: 10000 });
    await page.waitForSelector("#npListen", { state: "visible", timeout: 10000 });

    // 预填可能异步 (拉 /api/fleet 后回填): 最长等 5s 出现非空值, 否则按空报告
    let prefill = "";
    try {
      await page.waitForFunction(
        () => {
          const el = document.querySelector("#npListen");
          return el && String(el.value || "").trim() !== "";
        },
        { timeout: 5000, polling: 100 }
      );
    } catch (_) { /* 未回填 = 预填缺失, 原样报告 */ }
    prefill = await page.inputValue("#npListen");

    await page.click("#btnDlgX"); // 只关弹窗, 不创建
    console.log(JSON.stringify({ ok: true, prefill }));
  } finally {
    await browser.close();
  }
})().catch((e) => {
  console.log(JSON.stringify({ ok: false, error: String(e && e.message || e) }));
  process.exit(2);
});
