//! 跨平台「系统默认浏览器打开 URL」(共用小工具)。
//!
//! 调用方: gui.rs 托盘菜单「在浏览器打开控制台」/ FR-16 单实例提示 (ADR-8/13),
//! 以及 fleet.rs 的 POST /api/open-console (ADR-21②: 壳内页面按钮的受信路径,
//! wry 拦截 window.open 的绕行; headless 下脚本同样可触发)。
//!
//! 不引入额外依赖; 打开失败静默 (调用方各自决定兜底文案), spawn 即返回不阻塞。

/// 用系统默认浏览器打开 `url`。
pub(crate) fn open_url(url: &str) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}
