//! FR-15 图标集成 (ADR-18③): 运行时装配层。
//!
//! 常量本体由 build.rs 生成 (OUT_DIR/embedded_icons.rs): **资产存在才嵌入**
//! —— CI 三平台不依赖美术文件; 缺失时 `None`, 消费方 (gui.rs) 回退程序内
//! 绘制的圆点, `/favicon.svg` 回退内置兜底 SVG。资产规格见 assets/README.md。
//!
//! 说明: 托盘/窗口 PNG 已在构建期解码为原始 RGBA (build.rs), 运行时零解码依赖;
//! exe 图标由 winres 在构建期嵌进 PE 资源, 运行时无感知。

include!(concat!(env!("OUT_DIR"), "/embedded_icons.rs"));

/// favicon 资产缺失时的兜底 (保证 `/favicon.svg` 端点永远可用; 桥+串口母题)。
const FALLBACK_FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#0e6db8"/><path d="M6 16h20" stroke="#fff" stroke-width="2.5" stroke-linecap="round"/><path d="M10 11v10M16 11v10M22 11v10" stroke="#fff" stroke-width="2.5" stroke-linecap="round"/></svg>"##;

/// favicon SVG 文本 (资产缺失 → 兜底)。
pub(crate) fn favicon_svg() -> &'static str {
    FAVICON_SVG.unwrap_or(FALLBACK_FAVICON_SVG)
}
