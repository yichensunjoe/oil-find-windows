//! 平台与路径规则：本机卷校验、配置目录、根目录比较、显示格式化。
//!
//! 这些函数不创建窗口，可以独立阅读。所有「拒绝」分支都服务于同一个目标：
//! 只处理本机磁盘上的真实目录，不打开索引范围之外的目标（UNC、映射盘、
//! 设备路径、重解析点）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE,
};

use crate::api::wide;

const DRIVE_UNKNOWN: u32 = 0;
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOTE: u32 = 4;

/// 用户配置目录：`%LOCALAPPDATA%\OilFind`。
///
/// 返回 `(config.json, index.bin, 启动提示)`；前两项为 `None` 表示降级为仅内存
/// 模式，此时第三个元素带有面向用户的原因说明。
pub fn config_paths() -> (Option<PathBuf>, Option<PathBuf>, Option<String>) {
    // 用 LOCALAPPDATA 而不是漫游配置目录：几百 MB 的索引绝不能被漫游配置
    // 同步到域服务器。
    let base = match std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        Some(base) => PathBuf::from(base),
        None => return memory_only("无法读取当前用户配置目录；本次仅使用内存。"),
    };
    if local_path(&base).is_err() {
        return memory_only("当前用户配置目录不在可用本机磁盘；为保持离线，本次仅使用内存。");
    }
    let resolved = match resolve(&base) {
        Ok(path) => path,
        Err(err) => {
            return memory_only(&format!("无法验证当前用户配置目录；本次仅使用内存：{err}"))
        }
    };
    if local_path(&resolved).is_err() {
        return memory_only("当前用户配置目录实际位于网络位置；本次仅使用内存。");
    }
    let app_dir = base.join("OilFind");
    if let Err(err) = std::fs::create_dir_all(&app_dir) {
        return memory_only(&format!("无法创建当前用户配置目录；本次仅使用内存：{err}"));
    }
    let app_dir = match resolve(&app_dir) {
        Ok(path) => path,
        Err(_) => return memory_only("无法验证应用配置目录；本次仅使用内存。"),
    };
    if local_path(&app_dir).is_err() {
        return memory_only("应用配置目录不在本机磁盘；本次仅使用内存。");
    }
    (
        Some(app_dir.join("config.json")),
        Some(app_dir.join("index.bin")),
        None,
    )
}

fn memory_only(notice: &str) -> (Option<PathBuf>, Option<PathBuf>, Option<String>) {
    (None, None, Some(notice.to_string()))
}

/// 只接受「盘符 + 反斜杠」开头的本机路径，并校验卷类型。
pub fn local_path(path: &Path) -> Result<(), String> {
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
    local_path(path)?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| format!("目录路径无效：{err}"))?
            .join(path)
    };
    let resolved = resolve(&absolute).map_err(|err| format!("目录不存在或无法访问：{err}"))?;
    local_path(&resolved)?;
    let metadata = std::fs::metadata(&resolved).map_err(|err| format!("读取目录失败：{err}"))?;
    if !metadata.is_dir() {
        return Err("请选择本机目录，而不是单个文件".to_string());
    }
    Ok(resolved)
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

/// 是否为链接或重解析点（junction 在 Windows 上也按符号链接处理）。
pub fn is_link_or_reparse(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type().is_symlink(),
        Err(_) => false,
    }
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
    let local = seconds - i64::from(local_offset_minutes()) * 60;
    let days = local.div_euclid(86_400);
    let rest = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = rest / 3_600;
    let minute = (rest % 3_600) / 60;
    let second = rest % 60;
    if with_seconds {
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
    } else {
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
    }
}

/// 当前时区相对 UTC 的偏移分钟数（东八区为 +480）。
///
/// 读注册表里的 `ActiveTimeBias`，避免为此引入日期库或额外 Win32 feature；
/// 它反映当前生效的偏移，因此对不实行夏令时的时区（含中国）始终准确。
fn local_offset_minutes() -> i32 {
    static OFFSET: OnceLock<i32> = OnceLock::new();
    *OFFSET.get_or_init(|| read_active_bias().map(|bias| -bias).unwrap_or(0))
}

fn read_active_bias() -> Option<i32> {
    const KEY: &str = "SYSTEM\\CurrentControlSet\\Control\\TimeZoneInformation";
    let subkey = wide(KEY);
    let name = wide("ActiveTimeBias");
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if status != 0 || key.is_null() {
        return None;
    }
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let mut kind: u32 = 0;
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            (&mut value as *mut u32).cast::<u8>(),
            &mut size,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    // REG_DWORD = 4
    if status == 0 && kind == 4 && size == 4 {
        Some(value as i32)
    } else {
        None
    }
}

/// Howard Hinnant 的 civil_from_days：把 Unix 天数转成公历年月日。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_like_the_go_client() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024 * 3), "3.0 MB");
    }

    #[test]
    fn converts_unix_days_to_civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
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
