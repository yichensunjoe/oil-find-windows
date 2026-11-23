//! Win32 调用的薄封装。
//!
//! 所有函数都要求调用方提供**活到调用结束**的 UTF-16 缓冲区：这里统一用
//! `Vec<u16>` 局部变量 + `as_ptr()`，由 Rust 的作用域保证生命周期，绝不保存
//! 指向临时缓冲的裸指针。

use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
#[cfg(any(not(windows), test))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{GlobalFree, HWND, LPARAM, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HFONT};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::Shell::{
    SHBrowseForFolderW, SHGetPathFromIDListEx, SHOpenFolderAndSelectItems, SHParseDisplayName,
    ShellExecuteW, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetWindowTextLengthW, GetWindowTextW, MessageBoxW, SendMessageW,
    SetWindowTextW, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, SW_SHOWNORMAL, WM_SETFONT,
};

/// 剪贴板文本格式（CF_UNICODETEXT）。
const CF_UNICODETEXT_U32: u32 = 13;
/// GMEM_MOVEABLE
const GMEM_MOVEABLE: u32 = 0x0002;

/// An exclusive handle in the resolved configuration directory. Windows file
/// sharing rules apply across logon sessions, and the OS releases the handle
/// after a crash. Different users with different config directories do not
/// contend. Never unlink this file: that would let another process lock a new
/// file while the old handle is still open.
pub struct StorageGuard {
    _file: File,
}

impl StorageGuard {
    pub fn acquire(config_path: &Path) -> Result<Self, String> {
        let parent = config_path
            .parent()
            .ok_or_else(|| "配置路径无效，无法保护共享配置".to_string())?;
        let path = parent.join(".oilfind.lock");
        #[cfg(not(windows))]
        if crate::platform::is_link_or_reparse(&path) {
            return Err("应用配置锁是链接或无法访问".to_string());
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
            // No share flags: no other process can open, replace, or remove
            // the lock while this handle exists. Open a reparse point itself,
            // never its target, and reject it using handle metadata below.
            options
                .share_mode(0)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(0x100); // O_NOFOLLOW
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(0x20000); // O_NOFOLLOW
        }
        let file = options.open(&path).map_err(|err| {
            if matches!(err.raw_os_error(), Some(32 | 33)) && cfg!(windows) {
                "当前配置已被另一个 Oil Find 窗口使用，请先关闭该窗口。".to_string()
            } else {
                format!("无法保护当前用户配置，未启动：{err}")
            }
        })?;
        let metadata = file
            .metadata()
            .map_err(|err| format!("无法验证应用配置锁，未启动：{err}"))?;
        if !metadata.is_file() || is_reparse_metadata(&metadata) {
            return Err("应用配置锁不是普通文件，未启动。".to_string());
        }
        lock_open_file(&file).map_err(|err| {
            if err.kind() == io::ErrorKind::WouldBlock {
                "当前配置已被另一个 Oil Find 窗口使用，请先关闭该窗口。".to_string()
            } else {
                format!("无法保护当前用户配置，未启动：{err}")
            }
        })?;
        Ok(Self { _file: file })
    }
}

fn is_reparse_metadata(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

// Windows obtains exclusivity through share_mode(0) above. On Unix hosts the
// equivalent advisory lock lets the same lifetime/contended-path logic be
// tested without starting the Windows GUI.
#[cfg(unix)]
fn lock_open_file(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    // SAFETY: fd is an open file owned by `file`; flock does not retain it.
    if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn lock_open_file(_file: &File) -> io::Result<()> {
    Ok(())
}

/// Write a complete configuration next to its destination, then atomically
/// replace the destination. A pre-existing link, including a dangling link,
/// must never be followed or overwritten.
pub fn write_configuration(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "配置路径无效"))?;
    if crate::platform::is_link_or_reparse(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "配置是链接或无法验证",
        ));
    }
    let mut temporary = ConfigTemp::create(parent)?;
    let file = temporary.file.as_mut().expect("temporary file is open");
    file.write_all(data)?;
    file.sync_all()?;
    if crate::platform::is_link_or_reparse(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "配置是链接或无法验证",
        ));
    }
    temporary.commit(path)
}

struct ConfigTemp {
    #[cfg(any(not(windows), test))]
    path: PathBuf,
    file: Option<File>,
    committed: bool,
}

impl ConfigTemp {
    fn create(parent: &Path) -> io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        for _ in 0..64 {
            let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".oilfind-config-{}-{nanos}-{sequence}.tmp",
                std::process::id()
            ));
            match open_config_temp(&path) {
                Ok(file) => {
                    return Ok(Self {
                        #[cfg(any(not(windows), test))]
                        path,
                        file: Some(file),
                        committed: false,
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "无法创建独占配置临时文件",
        ))
    }

    fn commit(&mut self, destination: &Path) -> io::Result<()> {
        #[cfg(windows)]
        rename_open_file(
            self.file.as_ref().expect("temporary file is open"),
            destination,
        )?;
        #[cfg(not(windows))]
        fs::rename(&self.path, destination)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for ConfigTemp {
    fn drop(&mut self) {
        if !self.committed {
            #[cfg(windows)]
            if let Some(file) = self.file.as_ref() {
                // If deletion by handle fails, leave the temporary file behind.
                // Removing by path after closing it could delete a replacement.
                let _ = mark_file_for_deletion(file);
            }
            drop(self.file.take());
            #[cfg(not(windows))]
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn open_config_temp(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Foundation::GENERIC_WRITE;
        use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_FLAG_OPEN_REPARSE_POINT};
        // DELETE permits an atomic rename by handle while share_mode(0) keeps
        // the newly created path from being replaced by another process.
        options
            .access_mode(GENERIC_WRITE | DELETE)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(windows)]
fn rename_open_file(file: &File, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO,
    };

    let name: Vec<u16> = destination.as_os_str().encode_wide().collect();
    let name_bytes = name
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|length| u32::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "配置路径过长"))?;
    let bytes = std::mem::size_of::<FILE_RENAME_INFO>() + name_bytes as usize;
    let buffer_size = u32::try_from(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "配置路径过长"))?;
    // FILE_RENAME_INFO has a variable-length UTF-16 tail; usize storage gives
    // the structure the required alignment and sufficient trailing space.
    let words = bytes.div_ceil(std::mem::size_of::<usize>());
    let mut buffer = vec![0usize; words];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: the aligned buffer contains the whole structure and name; the
    // file handle remains open with DELETE access for the duration of the call.
    let result = unsafe {
        (*info).Anonymous.ReplaceIfExists = true;
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = name_bytes;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
        SetFileInformationByHandle(
            file.as_raw_handle() as _,
            FileRenameInfo,
            info.cast(),
            buffer_size,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn mark_file_for_deletion(file: &File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
    };
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: the handle is still open with DELETE access and the structure
    // lives through the call. Cleanup remains best-effort on failure.
    unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as _,
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        ) != 0
    }
}

/// 可以在线程之间传递的窗口句柄包装。
///
/// 句柄本身是进程内的整数，跨线程只用于 `PostMessageW`（线程安全），
/// 因此手工实现 `Send`/`Sync` 是安全的；所有控件操作仍只发生在 UI 线程。
#[derive(Clone, Copy)]
pub struct SendHwnd(pub HWND);

// SAFETY: HWND 只是内核对象索引；这里仅在后台线程里用作 PostMessageW 的目标。
unsafe impl Send for SendHwnd {}
// SAFETY: 同上，句柄只读且不拥有任何 Rust 数据。
unsafe impl Sync for SendHwnd {}

impl SendHwnd {
    /// 唤醒 UI 线程。用方法而不是直接读 `.0`，这样闭包捕获的是整个包装类型，
    /// 不会因为 2021 edition 的字段级捕获而绕过上面的 `Send`/`Sync`。
    pub fn post(self, message: u32) {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(self.0, message, 0, 0);
        }
    }
}

/// 把字符串转成以 0 结尾的 UTF-16，供 Win32 的 `PCWSTR` 参数使用。
pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 把路径转成以 0 结尾的 UTF-16。路径来自已校验的本机卷，非 UTF-8 字节极罕见。
pub fn wide_path(path: &Path) -> Vec<u16> {
    wide(&path.to_string_lossy())
}

/// 读取控件文本。
pub fn text(hwnd: HWND) -> String {
    if hwnd.is_null() {
        return String::new();
    }
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return String::new();
    }
    let length = length.min(32_768) as usize;
    let mut buffer = vec![0u16; length + 1];
    let written = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    if written <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..written as usize])
}

/// 设置控件文本。
pub fn put_text(hwnd: HWND, value: &str) {
    if hwnd.is_null() {
        return;
    }
    let buffer = wide(value);
    unsafe {
        SetWindowTextW(hwnd, buffer.as_ptr());
    }
}

pub fn enable(hwnd: HWND, enabled: bool) {
    if hwnd.is_null() {
        return;
    }
    unsafe {
        EnableWindow(hwnd, i32::from(enabled));
    }
}

pub fn send_message(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    unsafe { SendMessageW(hwnd, message, wparam, lparam) }
}

pub fn set_font(hwnd: HWND, font: HFONT) {
    if hwnd.is_null() || font.is_null() {
        return;
    }
    send_message(hwnd, WM_SETFONT, font as usize, 1);
}

/// 创建界面字体（Microsoft YaHei UI），高度按 DPI 缩放。
pub fn create_font(dpi: i32, logical_height: i32, weight: i32) -> Result<HFONT, String> {
    let face = wide("Microsoft YaHei UI");
    let height = -(logical_height * dpi / 96);
    let font = unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            face.as_ptr(),
        )
    };
    if font.is_null() {
        return Err("创建界面字体失败".to_string());
    }
    Ok(font)
}

pub fn delete_font(font: HFONT) {
    if !font.is_null() {
        unsafe {
            DeleteObject(font);
        }
    }
}

/// 创建子控件。
// 参数直接对应 CreateWindowExW 的位置参数，逐个打包成结构体反而更难对照。
#[allow(clippy::too_many_arguments)]
pub fn create_control(
    parent: HWND,
    instance: *mut c_void,
    class: &str,
    label: &str,
    style: u32,
    ex_style: u32,
    id: usize,
    font: HFONT,
) -> Result<HWND, String> {
    const WS_CHILD: u32 = 0x4000_0000;
    const WS_VISIBLE: u32 = 0x1000_0000;
    let class_buffer = wide(class);
    let label_buffer = wide(label);
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            class_buffer.as_ptr(),
            label_buffer.as_ptr(),
            style | WS_CHILD | WS_VISIBLE,
            0,
            0,
            100,
            28,
            parent,
            id as *mut c_void,
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        let error = io::Error::last_os_error();
        return Err(format!("创建界面控件失败（{class}）：{error}"));
    }
    set_font(hwnd, font);
    Ok(hwnd)
}

/// 提示对话框（仅用于错误与安全提醒）。
pub fn warn(owner: HWND, message: &str) {
    let text_buffer = wide(message);
    let title_buffer = wide("Oil Find for Windows");
    unsafe {
        MessageBoxW(
            owner,
            text_buffer.as_ptr(),
            title_buffer.as_ptr(),
            MB_OK | MB_ICONWARNING,
        );
    }
}

/// 只读说明对话框（用于 `--license`）。
///
/// 单文件交付不附带独立许可文件，`--license` 是这个副本里唯一可读的许可入口。
pub fn info(owner: HWND, message: &str) {
    let text_buffer = wide(message);
    let title_buffer = wide("Oil Find for Windows");
    unsafe {
        MessageBoxW(
            owner,
            text_buffer.as_ptr(),
            title_buffer.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// 目录选择器。返回空字符串表示用户取消。
pub fn choose_folder(owner: HWND) -> Result<String, String> {
    let mut display = vec![0u16; 260];
    let title = wide("选择本机目录（不支持网络共享或网络映射盘）");
    let info = BROWSEINFOW {
        hwndOwner: owner,
        pidlRoot: std::ptr::null_mut(),
        pszDisplayName: display.as_mut_ptr(),
        lpszTitle: title.as_ptr(),
        ulFlags: BIF_NEWDIALOGSTYLE | BIF_RETURNONLYFSDIRS,
        lpfn: None,
        lParam: 0,
        iImage: 0,
    };
    let pidl = unsafe { SHBrowseForFolderW(&info) };
    if pidl.is_null() {
        return Ok(String::new());
    }
    let mut buffer = vec![0u16; 32_768];
    let ok = unsafe { SHGetPathFromIDListEx(pidl, buffer.as_mut_ptr(), buffer.len() as u32, 0) };
    unsafe {
        windows_sys::Win32::System::Com::CoTaskMemFree(pidl as *const c_void);
    }
    if ok == 0 {
        return Err("读取所选目录失败".to_string());
    }
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..end]))
}

/// 用系统默认程序打开文件或目录。
pub fn open_path(owner: HWND, path: &Path) -> Result<(), String> {
    let verb = wide("open");
    let target = wide_path(path);
    let result = unsafe {
        ShellExecuteW(
            owner,
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result <= 32 {
        return Err(format!(
            "无法打开所选项目（系统代码 {result}）。请检查默认打开程序与访问权限"
        ));
    }
    Ok(())
}

/// 在资源管理器中定位并选中该项目。
pub fn reveal_path(path: &Path) -> Result<(), String> {
    let target = wide_path(path);
    let mut pidl: *mut windows_sys::Win32::UI::Shell::Common::ITEMIDLIST = std::ptr::null_mut();
    let hr = unsafe {
        SHParseDisplayName(
            target.as_ptr(),
            std::ptr::null_mut(),
            &mut pidl,
            0,
            std::ptr::null_mut(),
        )
    };
    if hr < 0 || pidl.is_null() {
        return Err(format!("无法定位所选项目（系统代码 0x{:08x}）", hr as u32));
    }
    let hr = unsafe { SHOpenFolderAndSelectItems(pidl, 0, std::ptr::null(), 0) };
    unsafe {
        windows_sys::Win32::System::Com::CoTaskMemFree(pidl as *const c_void);
    }
    if hr < 0 {
        return Err(format!(
            "无法打开资源管理器（系统代码 0x{:08x}）",
            hr as u32
        ));
    }
    Ok(())
}

/// 把路径写入剪贴板（CF_UNICODETEXT）。
pub fn copy_to_clipboard(owner: HWND, value: &str) -> Result<(), String> {
    let buffer: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = buffer.len() * std::mem::size_of::<u16>();
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
    if handle.is_null() {
        return Err("分配剪贴板内容失败".to_string());
    }
    let outcome = (|| -> Result<(), String> {
        let pointer = unsafe { GlobalLock(handle) };
        if pointer.is_null() {
            return Err("访问剪贴板内容失败".to_string());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(buffer.as_ptr(), pointer.cast::<u16>(), buffer.len());
            GlobalUnlock(handle);
        }
        if unsafe { OpenClipboard(owner) } == 0 {
            return Err("打开剪贴板失败".to_string());
        }
        let result = (|| -> Result<(), String> {
            if unsafe { EmptyClipboard() } == 0 {
                return Err("更新剪贴板失败".to_string());
            }
            if unsafe { SetClipboardData(CF_UNICODETEXT_U32, handle) }.is_null() {
                return Err("复制路径失败".to_string());
            }
            Ok(())
        })();
        unsafe {
            CloseClipboard();
        }
        result
    })();
    if outcome.is_err() {
        // 未能交给剪贴板：自己释放全局内存；成功后所有权归系统。
        unsafe {
            GlobalFree(handle);
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            for _ in 0..64 {
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "oilfind-win-test-{}-{nanos}-{id}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(err) => panic!("cannot create test directory: {err}"),
                }
            }
            panic!("cannot create unique test directory");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn reacquire_after_drop(config: &Path) -> StorageGuard {
        // Another parallel test may be forking and briefly inherit a Unix file
        // descriptor before exec closes it. A genuine leaked guard still fails.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match StorageGuard::acquire(config) {
                Ok(guard) => return guard,
                Err(_) if std::time::Instant::now() < deadline => std::thread::yield_now(),
                Err(err) => panic!("storage lock was not released: {err}"),
            }
        }
    }

    #[test]
    fn storage_guard_is_scoped_and_released_when_last_owner_drops() {
        let first = TempDir::new();
        let second = TempDir::new();
        let first_config = first.0.join("config.json");
        let second_config = second.0.join("config.json");
        let guard = std::sync::Arc::new(StorageGuard::acquire(&first_config).unwrap());
        let worker = guard.clone();
        let other_user = StorageGuard::acquire(&second_config).unwrap();
        assert!(StorageGuard::acquire(&first_config).is_err());
        drop(guard);
        assert!(StorageGuard::acquire(&first_config).is_err());
        drop(worker);
        let reopened = reacquire_after_drop(&first_config);
        drop(reopened);
        drop(other_user);
    }

    #[test]
    fn storage_guard_blocks_another_process_and_releases_after_exit() {
        const MODE: &str = "OILFIND_TEST_LOCK_CHILD_MODE";
        const CONFIG: &str = "OILFIND_TEST_LOCK_CHILD_CONFIG";
        if let Ok(mode) = std::env::var(MODE) {
            let config = PathBuf::from(std::env::var_os(CONFIG).unwrap());
            if mode == "busy" {
                assert!(StorageGuard::acquire(&config).is_err());
                return;
            }
            assert_eq!(mode, "exit");
            let _guard = StorageGuard::acquire(&config).unwrap();
            // Skip Rust destructors; the operating system must release the lock.
            std::process::exit(0);
        }

        let dir = TempDir::new();
        let config = dir.0.join("config.json");
        let child = |mode: &str| {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "api::tests::storage_guard_blocks_another_process_and_releases_after_exit",
                ])
                .env(MODE, mode)
                .env(CONFIG, &config)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "child failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let guard = StorageGuard::acquire(&config).unwrap();
        child("busy");
        drop(guard);
        child("exit");
        let reopened = reacquire_after_drop(&config);
        drop(reopened);
    }

    #[cfg(unix)]
    #[test]
    fn storage_guard_rejects_a_link_without_touching_its_target() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new();
        let victim = dir.0.join("victim");
        fs::write(&victim, b"preserve").unwrap();
        symlink(&victim, dir.0.join(".oilfind.lock")).unwrap();
        assert!(StorageGuard::acquire(&dir.0.join("config.json")).is_err());
        assert_eq!(fs::read(victim).unwrap(), b"preserve");
    }

    #[test]
    fn configuration_is_replaced_without_leaving_a_temporary_file() {
        let dir = TempDir::new();
        let config = dir.0.join("config.json");
        fs::write(&config, b"old configuration").unwrap();
        write_configuration(&config, b"new configuration").unwrap();
        assert_eq!(fs::read(&config).unwrap(), b"new configuration");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(config).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn precreated_temporary_symlink_cannot_truncate_its_target() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new();
        let victim = dir.0.join("victim");
        let candidate = dir.0.join("candidate.tmp");
        fs::write(&victim, b"preserve").unwrap();
        symlink(&victim, &candidate).unwrap();
        assert_eq!(
            open_config_temp(&candidate).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&victim).unwrap(), b"preserve");
        assert!(fs::symlink_metadata(candidate)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn configuration_destination_symlink_is_not_followed_or_replaced() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new();
        let victim = dir.0.join("victim");
        let config = dir.0.join("config.json");
        fs::write(&victim, b"preserve").unwrap();
        symlink(&victim, &config).unwrap();
        assert!(write_configuration(&config, b"replacement").is_err());
        assert_eq!(fs::read(victim).unwrap(), b"preserve");
        assert!(fs::symlink_metadata(config)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(not(windows))]
    #[test]
    fn failed_commit_preserves_the_previous_regular_configuration() {
        let dir = TempDir::new();
        let config = dir.0.join("config.json");
        fs::write(&config, b"old configuration").unwrap();
        let mut temporary = ConfigTemp::create(&dir.0).unwrap();
        temporary
            .file
            .as_mut()
            .unwrap()
            .write_all(b"new configuration")
            .unwrap();
        let moved = dir.0.join("moved.tmp");
        fs::rename(&temporary.path, &moved).unwrap();
        assert!(temporary.commit(&config).is_err());
        drop(temporary);
        assert_eq!(fs::read(config).unwrap(), b"old configuration");
        fs::remove_file(moved).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_keeps_the_old_config_if_an_open_handle_blocks_replacement() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = TempDir::new();
        let config = dir.0.join("config.json");
        fs::write(&config, b"old configuration").unwrap();
        let existing = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&config)
            .unwrap();
        let mut temporary = ConfigTemp::create(&dir.0).unwrap();
        temporary
            .file
            .as_mut()
            .unwrap()
            .write_all(b"new configuration")
            .unwrap();
        assert!(temporary.commit(&config).is_err());
        drop(temporary);
        drop(existing);
        assert_eq!(fs::read(config).unwrap(), b"old configuration");
    }

    #[cfg(windows)]
    #[test]
    fn windows_temp_path_cannot_be_removed_or_replaced_while_open() {
        let dir = TempDir::new();
        let temporary = ConfigTemp::create(&dir.0).unwrap();
        assert!(fs::remove_file(&temporary.path).is_err());
        assert!(fs::rename(&temporary.path, dir.0.join("moved.tmp")).is_err());
        let path = temporary.path.clone();
        drop(temporary);
        assert!(!path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_retains_temp_if_handle_deletion_fails() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = TempDir::new();
        let path = dir.0.join("temporary.tmp");
        // This test handle intentionally lacks DELETE access so that the
        // handle-based disposition fails without relying on ACL changes.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        file.write_all(b"partial configuration").unwrap();
        assert!(!mark_file_for_deletion(&file));
        let temporary = ConfigTemp {
            path: path.clone(),
            file: Some(file),
            committed: false,
        };
        drop(temporary);
        assert_eq!(fs::read(path).unwrap(), b"partial configuration");
    }

    #[test]
    fn failed_commit_cleans_up_its_partial_temporary_file() {
        let dir = TempDir::new();
        let destination = dir.0.join("occupied");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("marker"), b"preserve").unwrap();
        let mut temporary = ConfigTemp::create(&dir.0).unwrap();
        temporary.file.as_mut().unwrap().write_all(b"new").unwrap();
        let temp_path = temporary.path.clone();
        assert!(temporary.commit(&destination).is_err());
        drop(temporary);
        assert!(!temp_path.exists());
        assert_eq!(fs::read(destination.join("marker")).unwrap(), b"preserve");
    }
}
