//! Oil Find for Windows：原生 Win32 外壳。
//!
//! 非 Windows 平台只保留一个提示用的 `main`，但界面模块仍参与编译，这样
//! `cargo check`/`clippy` 在 macOS 上也能对 Win32 调用做类型检查（真正链接
//! 与运行仍只在 Windows 上）。
#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

mod api;
mod app;
mod content;
#[cfg(test)]
mod content_tests;
mod pdf_preview;
mod pdf_worker;
mod platform;
mod quick_panel;

// Embed licenses and notices with the single-file distribution.
#[used]
#[allow(dead_code)]
static EMBEDDED_LICENSES: [&str; 3] = [
    include_str!("../../../../LICENSE"),
    include_str!("../../../THIRD_PARTY_NOTICES.txt"),
    include_str!("../../../LICENSES/Content-search.txt"),
];

fn license_requested() -> bool {
    std::env::args_os().any(|argument| argument.to_string_lossy() == "--license")
}

/// Referencing the strings at runtime also keeps MSVC from discarding them.
fn licenses() -> String {
    format!(
        "Oil Find for Windows {}\n\n{}\n\n{}\n\n{}",
        env!("CARGO_PKG_VERSION"),
        EMBEDDED_LICENSES[0],
        EMBEDDED_LICENSES[1],
        EMBEDDED_LICENSES[2]
    )
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == pdf_worker::WORKER_ARGUMENT)
    {
        return pdf_worker::worker_main();
    }
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
