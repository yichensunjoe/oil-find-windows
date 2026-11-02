//! 平台与路径规则：本机卷校验、配置目录、根目录比较、显示格式化。
//!
//! 这些函数不创建窗口，可以独立阅读。所有「拒绝」分支都服务于同一个目标：
//! 只处理本机磁盘上的真实目录，不打开索引范围之外的目标（UNC、映射盘、
//! 设备路径、重解析点）。

use std::path::{Component, Path, PathBuf};

use chrono::{Local, TimeZone};
#[cfg(any(windows, test))]
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};

use crate::api::wide;

const DRIVE_UNKNOWN: u32 = 0;
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOTE: u32 = 4;

/// 用户配置目录：`%LOCALAPPDATA%\OilFind`。
///
/// 返回 `(config.json, index.json, 启动提示)`；前两项为 `None` 表示降级为仅内存
/// 模式，此时第三个元素带有面向用户的原因说明。
pub fn config_paths() -> (Option<PathBuf>, Option<PathBuf>, Option<String>) {
    // 用 LOCALAPPDATA 而不是漫游配置目录：几百 MB 的索引绝不能被漫游配置
    // 同步到域服务器。
    let base = match std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        Some(base) => PathBuf::from(base),
        None => return memory_only("无法读取当前用户配置目录；本次仅使用内存。"),
    };
    config_paths_for_base(&base)
}

fn config_paths_for_base(base: &Path) -> (Option<PathBuf>, Option<PathBuf>, Option<String>) {
    if validate_drive(base).is_err() {
        return memory_only("当前用户配置目录不在可用本机磁盘；为保持离线，本次仅使用内存。");
    }
    if let Err(err) = validate_directory_components(base) {
        return memory_only(&format!("无法验证当前用户配置目录；本次仅使用内存：{err}"));
    }
    if local_path(base).is_err() {
        return memory_only("当前用户配置目录实际位于网络位置；本次仅使用内存。");
    }
    let app_dir = match prepare_config_directory(base) {
        Ok(path) => path,
        Err(err) => {
            return memory_only(&format!(
                "无法验证或创建应用配置目录；本次仅使用内存：{err}"
            ))
        }
    };
    let app_dir = match resolve(&app_dir) {
        Ok(path) => path,
        Err(_) => return memory_only("无法验证应用配置目录；本次仅使用内存。"),
    };
    if local_path(&app_dir).is_err() {
        return memory_only("应用配置目录不在本机磁盘；本次仅使用内存。");
    }
    (
        Some(app_dir.join("config.json")),
        Some(app_dir.join("index.json")),
        None,
    )
}

// The parent must already exist. Never create through an unverified ancestor,
// and inspect an existing OilFind entry without following it.
fn prepare_config_directory(base: &Path) -> Result<PathBuf, String> {
    validate_directory_components(base)?;
    let app_dir = base.join("OilFind");
    match std::fs::symlink_metadata(&app_dir) {
        Ok(metadata) => {
            if metadata_is_link_or_reparse(&metadata) {
                return Err("不支持链接或重解析点，请选择真实本机目录".to_string());
            }
            if !metadata.is_dir() {
                return Err("应用配置目录不是目录".to_string());
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            create_config_directory(&app_dir)?;
            return Ok(app_dir);
        }
        Err(err) => return Err(format!("无法读取应用配置目录：{err}")),
    }
    validate_directory_components(&app_dir)?;
    Ok(app_dir)
}

// Another process may create the directory after the existence check. Whether
// we created it or lost that race, verify all components before reusing it.
fn create_config_directory(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "无法验证应用配置目录的父目录".to_string())?;
    validate_directory_components(parent)?;
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(format!("无法创建应用配置目录：{err}")),
    }
    validate_directory_components(path)
}

fn memory_only(notice: &str) -> (Option<PathBuf>, Option<PathBuf>, Option<String>) {
    (None, None, Some(notice.to_string()))
}

// Check only the drive root before inspecting any path components.
fn validate_drive(path: &Path) -> Result<(), String> {
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    if text.contains('\0')
        || text.starts_with("\\\\")
        || bytes.len() < 3
        || bytes[1] != b':'
        || (bytes[2] != b'\\' && bytes[2] != b'/')
        || !bytes[0].is_ascii_alphabetic()
    {
        return Err("仅支持带盘符的本机目录，不支持 UNC、设备路径或网络共享".to_string());
    }
    // 前两个字节已确认是 ASCII，切片一定落在字符边界上。
    let root = wide(&format!("{}\\", &text[..2]));
    let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
    if drive_type == DRIVE_REMOTE {
        return Err("不支持网络映射盘；请改选本机目录".to_string());
    }
    if drive_type == DRIVE_UNKNOWN || drive_type == DRIVE_NO_ROOT_DIR {
        return Err("所选盘符不可用，请选择可访问的本机目录".to_string());
    }
    Ok(())
}

/// Accept only absolute local drive paths and verify the volume type.
pub fn local_path(path: &Path) -> Result<(), String> {
    validate_drive(path)?;
    let text = path.to_string_lossy();
    // GetVolumePathNameW 不做长路径转换：超过 MAX_PATH 的路径会在这里失败，
    // 但上面的盘符检查已经证明卷类型，此时跳过更细的校验即可。
    if text.len() < 260 {
        let mut volume = vec![0u16; 32_768];
        let target = wide(&text);
        let ok = unsafe {
            GetVolumePathNameW(target.as_ptr(), volume.as_mut_ptr(), volume.len() as u32)
        };
        if ok == 0 {
            return Err("验证本机卷路径失败".to_string());
        }
        let drive_type = unsafe { GetDriveTypeW(volume.as_ptr()) };
        if drive_type == DRIVE_REMOTE
            || drive_type == DRIVE_UNKNOWN
            || drive_type == DRIVE_NO_ROOT_DIR
        {
            return Err("目录所在卷不是可用本机磁盘，不支持网络映射或共享".to_string());
        }
    }
    Ok(())
}

/// 校验并规范化一个索引根目录。
pub fn local_directory(path: &Path) -> Result<PathBuf, String> {
    validate_drive(path)?;
    validate_directory_components(path)?;
    let resolved = resolve(path).map_err(|err| format!("目录不存在或无法访问：{err}"))?;
    local_path(&resolved)?;
    let metadata = std::fs::metadata(&resolved).map_err(|err| format!("读取目录失败：{err}"))?;
    if !metadata.is_dir() {
        return Err("请选择本机目录，而不是单个文件".to_string());
    }
    Ok(resolved)
}

// Inspect each original component before any operation that follows it. Keep
// `..` in the sequence: resolving it first could hide a link that precedes it.
fn validate_directory_components(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("仅支持带盘符的本机目录，不支持相对路径".to_string());
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&current)
            .map_err(|err| format!("目录不存在或无法访问：{err}"))?;
        if metadata_is_link_or_reparse(&metadata) {
            return Err("不支持链接或重解析点，请选择真实本机目录".to_string());
        }
        if !metadata.is_dir() {
            return Err("请选择本机目录，而不是单个文件".to_string());
        }
    }
    Ok(())
}

/// 解析符号链接/联接点，并去掉 `\\?\` 前缀，得到可直接交给 Shell 的路径。
pub fn resolve(path: &Path) -> std::io::Result<PathBuf> {
    let resolved = std::fs::canonicalize(path)?;
    let text = resolved.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        if !rest.starts_with("UNC\\") {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(resolved)
}

/// Return true for links, reparse points, and errors other than NotFound.
pub fn is_link_or_reparse(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata_is_link_or_reparse(&metadata),
        Err(err) => err.kind() != std::io::ErrorKind::NotFound,
    }
}

fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || has_reparse_attribute(metadata.file_attributes())
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(any(windows, test))]
fn has_reparse_attribute(attributes: u32) -> bool {
    attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// `path` 是否等于 `root` 或位于其下（Windows 路径大小写不敏感）。
pub fn within_root(root: &Path, path: &Path) -> bool {
    let root = normalize(root);
    let path = normalize(path);
    if path == root {
        return true;
    }
    let mut prefix = root;
    if !prefix.ends_with('\\') {
        prefix.push('\\');
    }
    path.starts_with(&prefix)
}

/// 两个根目录集合是否完全相同（顺序无关）。
pub fn same_root_set(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left.iter().all(|root| {
            let target = normalize(Path::new(root));
            right
                .iter()
                .any(|other| normalize(Path::new(other)) == target)
        })
}

/// 同上，但用于 `Index::roots`（`Vec<PathBuf>`）与配置中的字符串列表比较。
pub fn same_root_paths(left: &[PathBuf], right: &[PathBuf]) -> bool {
    left.len() == right.len()
        && left.iter().all(|root| {
            right
                .iter()
                .any(|other| normalize(root) == normalize(other))
        })
}

fn normalize(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('/', "\\").to_lowercase();
    while text.len() > 3 && text.ends_with('\\') {
        text.pop();
    }
    text
}

/// 字节数转人类可读大小（与 Go 版 formatSize 一致）。
pub fn format_size(size: u64) -> String {
    if size < 1024 {
        return format!("{size} B");
    }
    let units = ["KB", "MB", "GB", "TB", "PB"];
    let mut value = size as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", units[unit])
}

/// 把 Unix 秒格式化为本机时间。`with_seconds` 控制是否带秒。
pub fn format_timestamp(seconds: i64, with_seconds: bool) -> String {
    let Some(timestamp) = Local.timestamp_opt(seconds, 0).single() else {
        return "无效时间".to_string();
    };
    let Some(local) = timestamp
        .naive_utc()
        .checked_add_offset(*timestamp.offset())
    else {
        return "无效时间".to_string();
    };
    let format = if with_seconds {
        "%Y-%m-%d %H:%M:%S"
    } else {
        "%Y-%m-%d %H:%M"
    };
    local.format(format).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let base = resolve(&std::env::temp_dir()).expect("temporary directory must exist");
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = base.join(format!("oilfind-win-{}-{nonce}-{id}", std::process::id()));
            std::fs::create_dir(&path).expect("create temporary test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn config_directory_is_created_and_existing_directory_reused() {
        let tmp = TestDir::new();
        let base = tmp.path().join("base");
        std::fs::create_dir(&base).unwrap();
        let expected = base.join("OilFind");

        assert_eq!(prepare_config_directory(&base).unwrap(), expected);
        assert!(std::fs::symlink_metadata(&expected).unwrap().is_dir());
        assert_eq!(prepare_config_directory(&base).unwrap(), expected);
        assert!(create_config_directory(&expected).is_ok());
    }

    #[test]
    fn concurrent_config_directory_creation_reuses_one_directory() {
        let tmp = TestDir::new();
        let base = tmp.path().join("base");
        std::fs::create_dir(&base).unwrap();
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        prepare_config_directory(&base)
                    })
                })
                .collect();
            for handle in handles {
                assert_eq!(handle.join().unwrap().unwrap(), base.join("OilFind"));
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn config_directory_rejects_links_and_missing_or_invalid_parents() {
        use std::os::unix::fs::symlink;

        let tmp = TestDir::new();
        let target = tmp.path().join("target");
        std::fs::create_dir_all(target.join("nested")).unwrap();
        let linked_base = tmp.path().join("linked-base");
        symlink(&target, &linked_base).unwrap();
        for base in [
            linked_base.clone(),
            linked_base.join("nested"),
            linked_base.join("..").join("target"),
        ] {
            let err = prepare_config_directory(&base).unwrap_err();
            assert!(err.contains("链接或重解析点"), "{base:?}: {err}");
        }
        assert!(!target.join("OilFind").exists());
        assert!(!target.join("nested").join("OilFind").exists());
        let err = create_config_directory(&linked_base.join("nested")).unwrap_err();
        assert!(err.contains("链接或重解析点"), "{err}");

        let base = tmp.path().join("base");
        std::fs::create_dir(&base).unwrap();
        symlink(&target, base.join("OilFind")).unwrap();
        let err = prepare_config_directory(&base).unwrap_err();
        assert!(err.contains("链接或重解析点"), "{err}");
        let err = create_config_directory(&base.join("OilFind")).unwrap_err();
        assert!(err.contains("链接或重解析点"), "{err}");
        assert!(!target.join("OilFind").exists());
        std::fs::remove_file(base.join("OilFind")).unwrap();
        symlink(tmp.path().join("missing-target"), base.join("OilFind")).unwrap();
        let err = prepare_config_directory(&base).unwrap_err();
        assert!(err.contains("链接或重解析点"), "{err}");
        std::fs::remove_file(base.join("OilFind")).unwrap();
        std::fs::write(base.join("OilFind"), b"test").unwrap();
        assert!(prepare_config_directory(&base).is_err());
        assert!(create_config_directory(&base.join("OilFind")).is_err());
        assert_eq!(std::fs::read(base.join("OilFind")).unwrap(), b"test");

        let missing = tmp.path().join("missing");
        assert!(prepare_config_directory(&missing).is_err());
        assert!(!missing.exists());
        assert!(prepare_config_directory(&base.join("OilFind").join("child")).is_err());
    }

    #[cfg(windows)]
    fn create_junction(link: &Path, target: &Path) {
        use std::os::windows::process::CommandExt;

        let cmd = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("cmd.exe");
        let output = std::process::Command::new(cmd)
            .args(["/D", "/V:OFF", "/C"])
            .raw_arg(r#"mklink /J "%OILFIND_TEST_LINK%" "%OILFIND_TEST_TARGET%""#)
            .env("OILFIND_TEST_LINK", link)
            .env("OILFIND_TEST_TARGET", target)
            .output()
            .expect("create temporary junction");
        assert!(
            output.status.success(),
            "junction creation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(windows)]
    #[test]
    fn config_paths_reject_junctions_without_creating_through_them() {
        for path in [
            r"\\server\share",
            r"\\?\C:\device",
            r"relative\dir",
            r"C:relative",
        ] {
            let (config, cache, notice) = config_paths_for_base(Path::new(path));
            assert!(config.is_none() && cache.is_none() && notice.is_some());
        }
        let tmp = TestDir::new();
        let base = tmp.path().join("base");
        let target = tmp.path().join("target");
        std::fs::create_dir(&base).unwrap();
        std::fs::create_dir(&target).unwrap();
        let (config, cache, notice) = config_paths_for_base(&base);
        assert!(config.is_some() && cache.is_some() && notice.is_none());
        assert!(create_config_directory(&base.join("OilFind")).is_ok());

        let linked_base = tmp.path().join("linked-base");
        create_junction(&linked_base, &target);
        let (config, cache, notice) = config_paths_for_base(&linked_base);
        assert!(config.is_none() && cache.is_none() && notice.is_some());
        assert!(!target.join("OilFind").exists());

        let other_base = tmp.path().join("other-base");
        std::fs::create_dir(&other_base).unwrap();
        create_junction(&other_base.join("OilFind"), &target);
        let (config, cache, notice) = config_paths_for_base(&other_base);
        assert!(config.is_none() && cache.is_none() && notice.is_some());
        assert!(create_config_directory(&other_base.join("OilFind")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn link_detection_distinguishes_missing_from_other_metadata_errors() {
        let tmp = TestDir::new();
        let file = tmp.path().join("file");
        std::fs::write(&file, b"test").unwrap();

        assert!(!is_link_or_reparse(&file));
        assert!(!is_link_or_reparse(&tmp.path().join("missing")));
        assert!(is_link_or_reparse(&file.join("child")));
    }

    #[test]
    fn reparse_attribute_is_detected_independently_of_symlink_type() {
        assert!(!has_reparse_attribute(0));
        assert!(!has_reparse_attribute(0x10));
        assert!(has_reparse_attribute(FILE_ATTRIBUTE_REPARSE_POINT));
        assert!(has_reparse_attribute(FILE_ATTRIBUTE_REPARSE_POINT | 0x10));
    }

    #[cfg(unix)]
    #[test]
    fn directory_validation_rejects_links_before_following_ancestors() {
        use std::os::unix::fs::symlink;

        let tmp = TestDir::new();
        let target = tmp.path().join("target");
        let child = target.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let link = tmp.path().join("link");
        symlink(&target, &link).unwrap();
        let dangling = tmp.path().join("dangling");
        symlink(tmp.path().join("missing-target"), &dangling).unwrap();

        assert!(!is_link_or_reparse(&target));
        assert!(is_link_or_reparse(&link));
        assert!(is_link_or_reparse(&dangling));
        assert!(validate_directory_components(&child).is_ok());
        assert!(validate_directory_components(&child.join("..").join("child")).is_ok());
        for path in [
            link.clone(),
            link.join("child"),
            link.join("missing"),
            link.join("..").join("target"),
            dangling,
        ] {
            let err = validate_directory_components(&path).unwrap_err();
            assert!(err.contains("链接或重解析点"), "{path:?}: {err}");
        }

        let file = tmp.path().join("file");
        std::fs::write(&file, b"test").unwrap();
        let err = validate_directory_components(&file.join("child")).unwrap_err();
        assert!(err.contains("单个文件"), "{err}");
        let err = validate_directory_components(&tmp.path().join("missing")).unwrap_err();
        assert!(err.contains("目录不存在或无法访问"), "{err}");
        assert!(validate_directory_components(Path::new("relative")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn local_directory_rejects_junction_on_any_component() {
        let tmp = TestDir::new();
        let target = tmp.path().join("target");
        std::fs::create_dir_all(target.join("child")).unwrap();
        assert_eq!(local_directory(&target).unwrap(), resolve(&target).unwrap());

        let junction = tmp.path().join("junction");
        create_junction(&junction, &target);
        assert!(is_link_or_reparse(&junction));
        for path in [
            junction.clone(),
            junction.join("child"),
            junction.join("missing"),
            junction.join("..").join("target"),
        ] {
            let err = local_directory(&path).unwrap_err();
            assert!(err.contains("链接或重解析点"), "{path:?}: {err}");
        }
    }

    #[test]
    fn formats_sizes_like_the_go_client() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024 * 3), "3.0 MB");
    }

    #[test]
    fn out_of_range_timestamps_do_not_panic() {
        assert_eq!(format_timestamp(i64::MIN, false), "无效时间");
        assert_eq!(format_timestamp(i64::MAX, true), "无效时间");
    }

    // A child process sets TZ before chrono initializes; other tests never
    // share a mutated process-wide environment or timezone cache.
    #[cfg(unix)]
    #[test]
    fn formats_local_time_with_historical_timezone_rules() {
        use chrono::{TimeZone, Utc};

        const CASE_ENV: &str = "OILFIND_PLATFORM_TIME_TEST";
        let utc = |year, month, day, hour, minute, second| {
            Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
                .single()
                .unwrap()
                .timestamp()
        };
        match std::env::var(CASE_ENV).as_deref() {
            Ok("Asia/Shanghai") => {
                assert_eq!(format_timestamp(0, true), "1970-01-01 08:00:00");
                assert_eq!(format_timestamp(-1, true), "1970-01-01 07:59:59");
                assert_eq!(
                    format_timestamp(chrono::DateTime::<Utc>::MAX_UTC.timestamp(), true),
                    "无效时间"
                );
                assert_eq!(
                    format_timestamp(utc(2024, 1, 1, 17, 30, 41), false),
                    "2024-01-02 01:30"
                );
                assert_eq!(
                    format_timestamp(utc(1991, 6, 1, 0, 0, 0), true),
                    "1991-06-01 09:00:00"
                );
            }
            Ok("America/New_York") => {
                for (time, expected) in [
                    (utc(2024, 3, 10, 6, 59, 59), "2024-03-10 01:59:59"),
                    (utc(2024, 3, 10, 7, 0, 0), "2024-03-10 03:00:00"),
                    (utc(2024, 11, 3, 5, 59, 59), "2024-11-03 01:59:59"),
                    (utc(2024, 11, 3, 6, 0, 0), "2024-11-03 01:00:00"),
                    (utc(2006, 3, 20, 12, 0, 0), "2006-03-20 07:00:00"),
                    (utc(2007, 3, 20, 12, 0, 0), "2007-03-20 08:00:00"),
                ] {
                    assert_eq!(format_timestamp(time, true), expected);
                }
            }
            _ => {
                for zone in ["Asia/Shanghai", "America/New_York"] {
                    let output = std::process::Command::new(std::env::current_exe().unwrap())
                        .args([
                            "--exact",
                            "platform::tests::formats_local_time_with_historical_timezone_rules",
                            "--nocapture",
                        ])
                        .env("TZ", zone)
                        .env(CASE_ENV, zone)
                        .output()
                        .expect("run timezone test in a child process");
                    assert!(
                        output.status.success()
                            && String::from_utf8_lossy(&output.stdout)
                                .contains("1 passed; 0 failed"),
                        "timezone {zone}:\n{}\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }
    }

    // 需要真实 Win32 卷查询，只能在 Windows 上运行。
    #[cfg(windows)]
    #[test]
    fn rejects_unc_and_relative_paths() {
        assert!(local_path(Path::new(r"\\server\share")).is_err());
        assert!(local_path(Path::new("relative\\dir")).is_err());
        assert!(local_path(Path::new(r"C:no-slash")).is_err());
    }

    #[test]
    fn root_membership_is_case_insensitive_and_boundary_aware() {
        let root = Path::new(r"C:\Data");
        assert!(within_root(root, Path::new(r"c:\data\file.txt")));
        assert!(within_root(root, Path::new(r"C:\Data")));
        assert!(!within_root(root, Path::new(r"C:\Database\file.txt")));
    }

    #[test]
    fn root_sets_compare_without_order() {
        let left = vec![r"C:\A".to_string(), r"D:\B".to_string()];
        let right = vec![r"d:\b".to_string(), r"c:\a".to_string()];
        assert!(same_root_set(&left, &right));
        assert!(!same_root_set(&left, &[r"C:\A".to_string()]));
    }
}
