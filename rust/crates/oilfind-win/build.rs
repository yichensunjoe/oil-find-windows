//! 只在 Windows 上把 `assets/app.manifest` 编译进 exe。
//!
//! 清单声明了 Common-Controls v6、asInvoker 与 PerMonitorV2；`scripts/verify-pe.py`
//! 会校验这些内容确实出现在产物的资源节里。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/app.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    #[cfg(windows)]
    {
        let _ = embed_resource::compile(&manifest, embed_resource::NONE);
    }
}
