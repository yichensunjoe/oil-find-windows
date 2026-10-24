//! 把 `assets/app.manifest` 编译进 Windows 产物。
//!
//! 清单声明了 Common-Controls v6、asInvoker 与 PerMonitorV2；`scripts/verify-pe.py`
//! 会校验这些内容确实出现在产物的资源节里。
//!
//! `embed_resource::compile` 接受的是 **.rc 脚本**而不是 manifest 本身，所以这里
//! 先在 `OUT_DIR` 生成一行 `1 24 "app.manifest"` 的脚本再编译；直接把 XML 交给
//! rc.exe 会得到 "RC.EXE failed to compile specified resource file"。
//!
//! 非 Windows 目标或找不到资源编译器时降级为 cargo 告警，这样在没有 Windows SDK
//! 的机器上 `cargo check --target x86_64-pc-windows-msvc` 仍能通过；真正缺清单的
//! 产物由 verify-pe.py 拦截。

use embed_resource::CompilationResult;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/app.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let script = out_dir.join("app.rc");
    let contents = format!("1 24 \"{}\"\n", manifest.display());
    if let Err(err) = fs::write(&script, contents) {
        println!("cargo:warning=无法生成资源脚本：{err}");
        return;
    }

    match embed_resource::compile(&script, embed_resource::NONE) {
        CompilationResult::Ok | CompilationResult::NotWindows => {}
        skipped => println!(
            "cargo:warning=未能嵌入 app.manifest（{skipped}）；该产物无法通过 verify-pe.py 校验"
        ),
    }
}
