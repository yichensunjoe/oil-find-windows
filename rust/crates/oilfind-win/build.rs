//! 把 `assets/app.manifest` 编译进 Windows 产物。
//!
//! 清单声明了 Common-Controls v6、asInvoker 与 PerMonitorV2；`scripts/verify-pe.py`
//! 会校验这些内容确实出现在产物的资源节里。
//!
//! `embed_resource` 自己按**目标平台**判断：非 Windows 目标直接返回 `NotWindows`，
//! 找不到资源编译器（rc.exe / llvm-rc / windres）时返回 `NotAttempted`。两种「没嵌
//! 进去」的情况都降级为 cargo 告警，这样 `cargo check --target x86_64-pc-windows-msvc`
//! 在没有 Windows SDK 的机器上也能通过；真正缺清单的产物由 verify-pe.py 拦截。

use embed_resource::CompilationResult;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/app.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());

    match embed_resource::compile(&manifest, embed_resource::NONE) {
        CompilationResult::Ok | CompilationResult::NotWindows => {}
        skipped => println!(
            "cargo:warning=未能嵌入 app.manifest（{skipped}）；该产物无法通过 verify-pe.py 校验"
        ),
    }
}
