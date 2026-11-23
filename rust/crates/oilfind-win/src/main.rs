//! Oil Find for Windows：原生 Win32 外壳。
//!
//! 非 Windows 平台只保留一个提示用的 `main`，但界面模块仍参与编译，这样
//! `cargo check`/`clippy` 在 macOS 上也能对 Win32 调用做类型检查（真正链接
//! 与运行仍只在 Windows 上）。
#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

mod api;
mod app;
mod platform;

// 单文件交付：MIT 许可与第三方 crate 声明必须随二进制一起分发，因此编入 exe。
// `#[used]` 阻止链接器裁掉这两段只读数据（构建脚本不再输出独立许可文件）。
#[used]
#[allow(dead_code)]
static EMBEDDED_LICENSES: [&str; 2] = [
    include_str!("../../../../LICENSE"),
    include_str!("../../../THIRD_PARTY_NOTICES.txt"),
];

fn license_requested() -> bool {
    std::env::args_os().any(|argument| argument.to_string_lossy() == "--license")
}

/// 许可文本必须真的被引用，否则 MSVC 链接器会把 `#[used]` 静态数据裁掉。
/// `--license` 既满足 MIT 的可读入口，也保证两段文本留在 exe 里。
fn licenses() -> String {
    format!(
        "Oil Find for Windows {}\n\n{}\n\n{}",
        env!("CARGO_PKG_VERSION"),
        EMBEDDED_LICENSES[0],
        EMBEDDED_LICENSES[1]
    )
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    if license_requested() {
        api::info(std::ptr::null_mut(), &licenses());
        return ExitCode::SUCCESS;
    }
    match app::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            api::warn(std::ptr::null_mut(), &err);
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() {
    if license_requested() {
        println!("{}", licenses());
        return;
    }
    println!("Oil Find for Windows 是 Windows 原生客户端，只能在 Windows 上运行。");
    println!("请在 Windows 上构建：cargo build --release -p oilfind-win");
}
