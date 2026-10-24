//! oilfind-cli：显式扫描与缓存查询的命令行入口。
//!
//! 设计约束（与 Windows 客户端一致）：
//! - 只有显式传入 `-scan` 才会扫描磁盘；`-root` 单独出现视为用法错误。
//! - 扫描被中断（Ctrl+C）时不提交缓存，退出码非 0。
//! - 输出为 JSON：`{"total":N,"entries":[...]}`。

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use oilfind_core::{scan, Entry, Index};

const DEFAULT_LIMIT: usize = 100;

/// 扫描取消标志：由 Ctrl+C 处理器置位，`scan` 轮询它。
static CANCELLED: AtomicBool = AtomicBool::new(false);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "{err}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug)]
struct Options {
    scan: bool,
    roots: Vec<PathBuf>,
    cache: PathBuf,
    query: String,
    limit: usize,
}

fn run() -> Result<(), String> {
    let options = parse_options(std::env::args().skip(1))?;
    install_cancel_handler();
    let cancel = &CANCELLED;

    let index = if options.scan {
        let progress = |p: oilfind_core::Progress| {
            if p.visited.is_multiple_of(5000) {
                let _ = writeln!(
                    std::io::stderr(),
                    "扫描中：已访问 {} 项，跳过 {} 项",
                    p.visited,
                    p.skipped
                );
            }
        };
        let index = scan(cancel, &options.roots, progress).map_err(|err| err.to_string())?;
        // 取消后不提交缓存：避免半截快照覆盖上一份完整索引。
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消：未写入索引缓存".to_string());
        }
        if let Some(parent) = options.cache.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| format!("创建缓存目录 {} 失败：{err}", parent.display()))?;
            }
        }
        index
            .save_cancellable(cancel, &options.cache)
            .map_err(|err| format!("写入索引缓存 {} 失败：{err}", options.cache.display()))?;
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消：未提交索引缓存".to_string());
        }
        index
    } else {
        Index::load(&options.cache)
            .map_err(|err| format!("读取索引缓存 {} 失败：{err}", options.cache.display()))?
    };

    let (entries, total) = index
        .search(&options.query, None, options.limit)
        .map_err(|err| format!("搜索条件有误：{err}"))?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    write_report(&mut out, total, &entries).map_err(|err| format!("写入结果失败：{err}"))
}

fn parse_options(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut scan = false;
    let mut roots = Vec::new();
    let mut cache: Option<PathBuf> = None;
    let mut query = String::new();
    let mut limit = DEFAULT_LIMIT;
    let mut args = args.peekable();

    while let Some(raw) = args.next() {
        if raw == "--" {
            break;
        }
        if !raw.starts_with('-') {
            return Err(format!("无法识别的参数：{raw}"));
        }
        // 同时接受 -flag 与 --flag，便于脚本使用。
        let flag = raw.trim_start_matches('-');
        match flag {
            "scan" => scan = true,
            "h" | "help" => {
                print_usage();
                std::process::exit(0);
            }
            "root" => {
                let value = args
                    .next()
                    .ok_or_else(|| "-root 需要一个目录参数".to_string())?;
                if value.trim().is_empty() {
                    return Err("-root 的目录不能为空".to_string());
                }
                roots.push(PathBuf::from(value));
            }
            "cache" => {
                let value = args
                    .next()
                    .ok_or_else(|| "-cache 需要一个文件路径".to_string())?;
                if value.trim().is_empty() {
                    return Err("-cache 的路径不能为空".to_string());
                }
                cache = Some(PathBuf::from(value));
            }
            "query" => {
                query = args
                    .next()
                    .ok_or_else(|| "-query 需要一个搜索文本".to_string())?;
            }
            "limit" => {
                let value = args
                    .next()
                    .ok_or_else(|| "-limit 需要一个数字".to_string())?;
                limit = value
                    .parse::<usize>()
                    .map_err(|_| format!("-limit 需要一个正整数，收到：{value}"))?;
                if limit == 0 {
                    return Err("-limit 必须大于 0".to_string());
                }
            }
            other => return Err(format!("无法识别的参数：-{other}")),
        }
    }

    if scan && roots.is_empty() {
        return Err("扫描必须显式指定 -root，默认不会扫描任何个人目录".to_string());
    }
    if !scan && !roots.is_empty() {
        return Err("仅有 -root 不会启动扫描，请明确添加 -scan".to_string());
    }
    let cache = match cache {
        Some(path) => path,
        None => default_cache_path()?,
    };
    Ok(Options {
        scan,
        roots,
        cache,
        query,
        limit,
    })
}

fn print_usage() {
    let text = "\
用法：oilfind-cli [选项]

  -scan            为 -root 指定的目录建立元数据索引（必须显式开启）
  -root <目录>     要索引的本机目录，可重复
  -cache <文件>    索引缓存文件（默认：应用配置目录下的 index.bin）
  -query <文本>    在索引中搜索；可与 -scan 同时使用
  -limit <n>       最多返回多少条结果（默认 100）
  -h, --help       显示本帮助

输出为 JSON：{\"total\":N,\"entries\":[{\"path\":...,\"name\":...,\"size\":...,\"modified\":...,\"is_dir\":...}]}
";
    let _ = write!(std::io::stdout(), "{text}");
}

/// 默认缓存路径：Windows 用 `%LOCALAPPDATA%\OilFind\index.bin`，
/// 其它平台用 `$XDG_CACHE_HOME` 或 `~/.cache/oilfind/index.bin`。
fn default_cache_path() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        if let Some(base) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
            return Ok(PathBuf::from(base).join("OilFind").join("index.bin"));
        }
        Err("无法读取 LOCALAPPDATA；请用 -cache 显式指定缓存文件".to_string())
    }
    #[cfg(not(windows))]
    {
        if let Some(base) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
            return Ok(PathBuf::from(base).join("oilfind").join("index.bin"));
        }
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| "无法读取 HOME；请用 -cache 显式指定缓存文件".to_string())?;
        Ok(PathBuf::from(home)
            .join(".cache")
            .join("oilfind")
            .join("index.bin"))
    }
}

fn write_report(out: &mut impl Write, total: usize, entries: &[Entry]) -> std::io::Result<()> {
    write!(out, "{{\"total\":{total},\"entries\":[")?;
    for (position, entry) in entries.iter().enumerate() {
        if position > 0 {
            out.write_all(b",")?;
        }
        write!(
            out,
            "{{\"path\":{},\"name\":{},\"size\":{},\"modified\":{},\"is_dir\":{}}}",
            json_string(&entry.path.to_string_lossy()),
            json_string(&entry.name),
            entry.size,
            entry.modified,
            entry.is_dir
        )?;
    }
    out.write_all(b"]}\n")
}

/// 最小 JSON 字符串转义：只处理 JSON 规范要求的字符，非 ASCII 原样输出（UTF-8 合法）。
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 安装 Ctrl+C 处理器：只置位取消标志，让扫描线程自行收尾。
fn install_cancel_handler() {
    #[cfg(windows)]
    {
        unsafe extern "system" fn handler(_: u32) -> i32 {
            CANCELLED.store(true, Ordering::SeqCst);
            1
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn SetConsoleCtrlHandler(
                handler: Option<unsafe extern "system" fn(u32) -> i32>,
                add: i32,
            ) -> i32;
        }
        // 失败时退回默认行为（进程直接终止），仍然不会提交缓存。
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
    #[cfg(not(windows))]
    {
        unsafe extern "C" fn handler(_: i32) {
            CANCELLED.store(true, Ordering::SeqCst);
        }
        extern "C" {
            fn signal(signum: i32, handler: usize) -> usize;
        }
        // SIGINT = 2；不引入 libc 依赖，直接声明 C 运行时的 signal()。
        unsafe {
            signal(2, handler as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escapes_quotes_and_controls() {
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("c:\\tmp"), "\"c:\\\\tmp\"");
        assert_eq!(json_string("换\n行"), "\"换\\n行\"");
        assert_eq!(json_string("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn root_without_scan_is_rejected() {
        let err = parse_options(["-root".to_string(), "/data".to_string()].into_iter())
            .expect_err("仅有 -root 必须报错");
        assert!(err.contains("-scan"), "{err}");
    }

    #[test]
    fn scan_without_root_is_rejected() {
        let err = parse_options(["-scan".to_string()].into_iter()).expect_err("必须指定 -root");
        assert!(err.contains("-root"), "{err}");
    }

    #[test]
    fn parses_scan_with_roots_and_limit() {
        let options = parse_options(
            [
                "-scan",
                "-root",
                "/data",
                "--root",
                "/more",
                "-query",
                "报表",
                "-limit",
                "7",
                "-cache",
                "/tmp/index.bin",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .expect("参数应可解析");
        assert!(options.scan);
        assert_eq!(
            options.roots,
            vec![PathBuf::from("/data"), PathBuf::from("/more")]
        );
        assert_eq!(options.query, "报表");
        assert_eq!(options.limit, 7);
        assert_eq!(options.cache, PathBuf::from("/tmp/index.bin"));
    }
}
