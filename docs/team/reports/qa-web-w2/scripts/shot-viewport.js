// qa-web-w2 三档分辨率截图 + 横向溢出检查 + 渲染态内容核对 (席位 707)
// 用法: NODE_PATH="$(npm root -g)" node shot-viewport.js
const { chromium } = require('playwright');
const path = require('path');
const OUT = 'C:/Users/liuyu/Desktop/WorkPlace/serialhub/docs/team/reports/qa-web-w2';
const URL = 'https://misakamikoto128.github.io/serialhub/';

(async () => {
  const browser = await chromium.launch();
  const results = [];
  for (const width of [360, 720, 1440]) {
    const page = await browser.newPage({ viewport: { width, height: 800 } });
    await page.goto(URL, { waitUntil: 'networkidle', timeout: 60000 });
    await page.waitForTimeout(2000); // 等新鲜度自检 fetch 完成
    const m = await page.evaluate(() => {
      const d = document.documentElement;
      const q = (s) => document.querySelector(s);
      return {
        scrollWidth: d.scrollWidth,
        clientWidth: d.clientWidth,
        bodyScrollWidth: document.body.scrollWidth,
        freshnessHidden: document.getElementById('dl-freshness')
          ? document.getElementById('dl-freshness').hidden : null,
        freshnessText: (document.getElementById('dl-freshness').textContent || '').trim(),
        renderedVersion: q('[data-rel="version"]') ? q('[data-rel="version"]').textContent : null,
        renderedDate: q('[data-rel="date"]') ? q('[data-rel="date"]').textContent : null,
        changelogHref: q('[data-rel="changelog"]') ? q('[data-rel="changelog"]').href : null,
        releasesHref: q('[data-rel="releases"]') ? q('[data-rel="releases"]').href : null,
        linkWindows: q('[data-rel="link-windows"]') ? q('[data-rel="link-windows"]').href : null,
        shaWindows: q('[data-rel="sha-windows"]') ? q('[data-rel="sha-windows"]').textContent : null,
        shaLinux: q('[data-rel="sha-linux"]') ? q('[data-rel="sha-linux"]').textContent : null,
        shaMacos: q('[data-rel="sha-macos"]') ? q('[data-rel="sha-macos"]').textContent : null,
      };
    });
    // 若有横向溢出, 定位溢出元素 (最多 10 个)
    m.overflowing = await page.evaluate(() => {
      const d = document.documentElement;
      const bad = [];
      if (d.scrollWidth > d.clientWidth) {
        document.querySelectorAll('*').forEach((el) => {
          const r = el.getBoundingClientRect();
          if ((r.right > d.clientWidth + 1 || r.left < -1) && bad.length < 10) {
            const cls = typeof el.className === 'string' && el.className
              ? '.' + el.className.split(' ')[0] : '';
            bad.push(el.tagName + (el.id ? '#' + el.id : '') + cls +
              ' left=' + Math.round(r.left) + ' right=' + Math.round(r.right));
          }
        });
      }
      return bad;
    });
    await page.screenshot({ path: path.join(OUT, `qa-web-w2-${width}.png`), fullPage: true });
    results.push(Object.assign({ width }, m));
    await page.close();
  }
  await browser.close();
  console.log(JSON.stringify(results, null, 2));
})().catch((e) => { console.error(e); process.exit(1); });
