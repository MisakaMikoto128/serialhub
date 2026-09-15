#!/usr/bin/env node
/* ==========================================================================
 * 主题记忆探针 (qa-xtest-config 第 5 组) — 黑盒真浏览器, 走 UI 用户路径
 *
 * 用法:  NODE_PATH="$(npm root -g)" node tests/pw_theme_persist.cjs <set|verify> <管理台URL> <profile目录>
 * 输出:  末行 JSON  {ok:true, ...}  /  {ok:false, error:"..."}
 *
 * 做法: Playwright 以 **持久化 profile** (launchPersistentContext) 开无头 Chromium:
 * - set:    点「设置」(#btnSettings) → #setTheme 选项出现 win95 (列表异步补齐) →
 *           selectOption 触发 change → UI 落 localStorage("sh_theme")=win95 并挂
 *           /themes/win95.css (FR-14 用户路径, 不直接写 localStorage);
 * - verify: **重新启动一个浏览器进程** (同一 profile 目录 = 同一浏览器存储),
 *           页面 <head> 同步脚本应从 localStorage 记忆挂回 win95.css ——
 *           即「换主题 → 浏览器重启 → 主题记忆生效」闭环。
 * 两模式都收集 pageerror (预期外 JS 错误 = 缺陷信号)。
 * ========================================================================== */
"use strict";
const { chromium } = require("playwright");

(async () => {
  const [mode, url, profileDir] = process.argv.slice(2);
  if (!mode || !url || !profileDir)
    throw new Error("usage: node pw_theme_persist.cjs <set|verify> <url> <profileDir>");
  if (mode !== "set" && mode !== "verify")
    throw new Error(`mode 越域: ${mode} (只认 set|verify)`);

  const ctx = await chromium.launchPersistentContext(profileDir, {
    headless: true,
    viewport: { width: 1280, height: 900 },
  });
  try {
    const page = await ctx.newPage();
    const pageErrors = [];
    page.on("pageerror", (e) => pageErrors.push(String((e && e.message) || e)));
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 20000 });

    if (mode === "set") {
      await page.click("#btnSettings");
      await page.waitForSelector("#dlgSettings[open]", { timeout: 10000 });
      // 主题下拉先摆当前值, 列表异步补齐 (openSettings → loadThemeList)
      await page.waitForFunction(
        () => {
          const sel = document.querySelector("#setTheme");
          return !!sel && Array.from(sel.options).some((o) => o.value === "win95");
        },
        { timeout: 10000, polling: 100 }
      );
      await page.selectOption("#setTheme", "win95"); // change → applyTheme("win95", remember=true)
      await page.waitForFunction(
        () => {
          try { return localStorage.getItem("sh_theme") === "win95"; }
          catch (_) { return false; }
        },
        { timeout: 5000, polling: 100 }
      );
      await page.waitForFunction(
        () => {
          const l = document.getElementById("themeCss");
          return !!l && String(l.getAttribute("href") || "").includes("win95.css");
        },
        { timeout: 5000, polling: 100 }
      );
      console.log(JSON.stringify({ ok: true, mode, stored: "win95", applied: "win95.css" }));
    } else {
      // verify: 全新浏览器进程 (同 profile)。head 内同步脚本按 localStorage 记忆挂主题,
      // domcontentloaded 时 #themeCss 应已指向 win95.css —— 记忆 → 重启 → 生效。
      await page.waitForFunction(
        () => {
          const l = document.getElementById("themeCss");
          return !!l && String(l.getAttribute("href") || "").includes("win95.css");
        },
        { timeout: 10000, polling: 100 }
      );
      const stored = await page.evaluate(() => {
        try { return localStorage.getItem("sh_theme"); } catch (_) { return null; }
      });
      console.log(JSON.stringify({ ok: true, mode, stored, applied: "win95.css" }));
    }
    if (pageErrors.length)
      console.error(JSON.stringify({ pageErrors: pageErrors.slice(0, 3) }));
    if (pageErrors.length) process.exit(3); // 预期外 JS 错误按失败报
  } finally {
    await ctx.close();
  }
})().catch((e) => {
  console.log(JSON.stringify({ ok: false, error: String((e && e.message) || e) }));
  process.exit(2);
});
