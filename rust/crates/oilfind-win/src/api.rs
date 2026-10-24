//! Win32 调用的薄封装。
//!
//! 所有函数都要求调用方提供**活到调用结束**的 UTF-16 缓冲区：这里统一用
//! `Vec<u16>` 局部变量 + `as_ptr()`，由 Rust 的作用域保证生命周期，绝不保存
//! 指向临时缓冲的裸指针。

use std::ffi::c_void;
use std::path::Path;

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
    SetWindowTextW, MB_ICONWARNING, MB_OK, SW_SHOWNORMAL, WM_SETFONT,
};

/// 剪贴板文本格式（CF_UNICODETEXT）。
const CF_UNICODETEXT_U32: u32 = 13;
/// GMEM_MOVEABLE
const GMEM_MOVEABLE: u32 = 0x0002;

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
pub fn create_font(dpi: i32) -> Result<HFONT, String> {
    let face = wide("Microsoft YaHei UI");
    let height = -(17 * dpi / 96);
    let font = unsafe { CreateFontW(height, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr()) };
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
        return Err(format!("创建界面控件失败（{class}）"));
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
