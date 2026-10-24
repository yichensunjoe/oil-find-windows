//! 主窗口：原生 Win32 界面、后台扫描/搜索线程、事件分发。
//!
//! 线程模型：
//! - UI 线程只做界面更新，**从不**等待后台任务（无 join、无阻塞 recv）。
//! - 扫描线程与搜索线程通过 `mpsc` 送回完成事件，再用 `PostMessageW` 唤醒 UI。
//! - 进度通过 `PostMessage(WM_APP + 2)` 通知，UI 线程读取最新的进度快照。

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use oilfind_core::{scan, Entry, Index, Progress};
use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetSysColorBrush, InvalidateRect, UpdateWindow, COLOR_WINDOW, HFONT,
};
use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, LVCF_TEXT, LVCOLUMNW,
    LVIF_TEXT, LVITEMW, LVN_ITEMCHANGED, NMHDR, NMITEMACTIVATE, NM_DBLCLK,
};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, GetKeyState, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetSystemMetrics, IsDialogMessageW, KillTimer, LoadCursorW, MoveWindow, PostQuitMessage,
    RegisterClassExW, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage,
    IDC_ARROW, MINMAXINFO, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW,
    WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_GETMINMAXINFO,
    WM_KEYDOWN, WM_NOTIFY, WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_CLIPCHILDREN, WS_EX_CONTROLPARENT,
    WS_OVERLAPPEDWINDOW,
};

use crate::api::{self, SendHwnd};
use crate::platform;

const RESULT_LIMIT: usize = 2000;
const MAX_ROOTS: usize = 64;
const SEARCH_DEBOUNCE_MS: u32 = 250;
const SYNC_INTERVAL_MS: u32 = 30_000;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);

// 控件 ID（与 Go 版一致）。
const ID_SEARCH: usize = 100;
const ID_ADD_ROOT: usize = 101;
const ID_REMOVE_ROOT: usize = 102;
const ID_START: usize = 103;
const ID_STOP: usize = 104;
const ID_ROOTS: usize = 105;
const ID_TYPES: usize = 106;
const ID_RESULTS: usize = 107;
const ID_OPEN: usize = 108;
const ID_REVEAL: usize = 109;
const ID_COPY: usize = 110;

// 自定义消息：进度用 WM_APP + n，避免与系统消息冲突。
const WM_APP_EVENT: u32 = WM_APP + 1;
const WM_APP_PROGRESS: u32 = WM_APP + 2;

const TIMER_SEARCH: usize = 1;
const TIMER_SYNC: usize = 2;

/// 控件样式与控件消息（数值来自 Win32 SDK，集中在此便于核对）。
mod style {
    pub const WS_TABSTOP: u32 = 0x0001_0000;
    pub const WS_BORDER: u32 = 0x0080_0000;
    pub const WS_VSCROLL: u32 = 0x0020_0000;
    pub const WS_EX_CLIENTEDGE: u32 = 0x0000_0200;
    pub const SS_LEFT: u32 = 0x0000_0080;
    pub const ES_AUTOHSCROLL: u32 = 0x0000_0080;
    pub const LBS_NOTIFY: u32 = 0x0000_0001;
    pub const LBS_NOINTEGRALHEIGHT: u32 = 0x0000_0100;
    pub const CBS_DROPDOWNLIST: u32 = 0x0000_0003;
    pub const LVS_REPORT: u32 = 0x0000_0001;
    pub const LVS_SINGLESEL: u32 = 0x0000_0004;
    pub const LVS_SHOWSELALWAYS: u32 = 0x0000_0008;
    pub const LVS_EX_FULLROWSELECT: u32 = 0x0000_0020;
    pub const LVS_EX_GRIDLINES: u32 = 0x0000_0001;
    pub const LVS_EX_DOUBLEBUFFER: u32 = 0x0001_0000;
    pub const LVCF_FMT: u32 = 0x0001;
    pub const LVCF_WIDTH: u32 = 0x0002;
    pub const LVCF_SUBITEM: u32 = 0x0008;
    pub const LVM_FIRST: u32 = 0x1000;
    pub const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = LVM_FIRST + 54;
    pub const LVM_INSERTCOLUMNW: u32 = LVM_FIRST + 97;
    pub const LVM_INSERTITEMW: u32 = LVM_FIRST + 77;
    pub const LVM_SETITEMW: u32 = LVM_FIRST + 76;
    pub const LVM_DELETEALLITEMS: u32 = LVM_FIRST + 9;
    pub const LVM_GETNEXTITEM: u32 = LVM_FIRST + 12;
    pub const LVM_SETCOLUMNWIDTH: u32 = LVM_FIRST + 30;
    pub const LVNI_SELECTED: usize = 0x0002;
    pub const CB_ADDSTRING: u32 = 0x0143;
    pub const CB_SETCURSEL: u32 = 0x014E;
    pub const CB_GETCURSEL: u32 = 0x0147;
    pub const LB_ADDSTRING: u32 = 0x0180;
    pub const LB_RESETCONTENT: u32 = 0x0184;
    pub const LB_GETCURSEL: u32 = 0x0188;
    pub const EM_SETLIMITTEXT: u32 = 0x00C5;
    pub const EM_SETCUEBANNER: u32 = 0x1501;
    pub const EM_SETSEL: u32 = 0x00B1;
    pub const WM_SETREDRAW: u32 = 0x000B;
}

// UI 线程持有的 App 指针。
//
// Win32 的窗口过程没有「用户数据」以外的上下文，所有消息都在同一个 UI 线程
// 上派发；这里沿用 Go 版的全局指针做法，避免在窗口过程里长期持有一个 `&mut`
// 借用（`SetWindowPos` 等调用会同步重入 WM_SIZE）。
thread_local! {
    static APP: Cell<*mut App> = const { Cell::new(std::ptr::null_mut()) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    let pointer = APP.with(Cell::get);
    if pointer.is_null() {
        return None;
    }
    // SAFETY: 指针在 run() 期间指向栈上的 Box<App>，只在 UI 线程访问；
    // 重入只发生在同一个线程的嵌套窗口消息里。
    Some(f(unsafe { &mut *pointer }))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // 后台线程 panic 不应让 UI 线程跟着崩：中毒后取回内部值继续用。
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Default, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cache_roots: Vec<String>,
}

struct ScanSuccess {
    index: Index,
    save_error: Option<String>,
    updated: SystemTime,
}

enum Event {
    Scan {
        generation: u64,
        outcome: Result<ScanSuccess, String>,
    },
    Cache {
        generation: u64,
        outcome: Result<(Index, SystemTime), String>,
    },
    Search {
        generation: u64,
        outcome: Result<(Vec<Entry>, usize), String>,
    },
}

struct SearchRequest {
    generation: u64,
    index: Arc<Index>,
    query: String,
    kind: Option<String>,
}

/// 容量为 1 的搜索请求信箱：新请求直接替换尚未开始执行的旧请求，
/// 因此 UI 线程永远不会因为搜索忙而阻塞。
struct SearchQueue {
    state: Mutex<SearchState>,
    ready: Condvar,
}

struct SearchState {
    request: Option<SearchRequest>,
    closed: bool,
}

struct App {
    hwnd: HWND,
    instance: HWND,
    font: HFONT,
    dpi: i32,
    search: HWND,
    roots_list: HWND,
    types: HWND,
    results: HWND,
    status: HWND,
    add: HWND,
    remove: HWND,
    start: HWND,
    stop: HWND,
    open: HWND,
    reveal: HWND,
    copy: HWND,
    labels: Vec<HWND>,
    config: Config,
    config_path: Option<PathBuf>,
    cache_path: Option<PathBuf>,
    index: Option<Arc<Index>>,
    rows: Vec<Entry>,
    scan_cancel: Option<Arc<AtomicBool>>,
    scan_gate: Arc<Mutex<()>>,
    scan_generation: u64,
    search_generation: u64,
    scanning: bool,
    auto_sync: bool,
    closing: bool,
    storage_notice: String,
    last_notice: String,
    startup_notice: String,
    init_error: Option<String>,
    last_update: Option<SystemTime>,
    last_progress: Option<Progress>,
    events_tx: Sender<Event>,
    events_rx: Receiver<Event>,
    progress_slot: Arc<Mutex<Option<Progress>>>,
    search_queue: Arc<SearchQueue>,
}

pub fn run() -> Result<(), String> {
    unsafe {
        set_dpi_awareness();
    }
    // COINIT_APARTMENTTHREADED：目录选择器需要 STA。
    let hr = unsafe { CoInitializeEx(std::ptr::null(), 0x2) };
    if hr < 0 {
        return Err(format!(
            "初始化目录选择器失败（系统代码 0x{:08x}）",
            hr as u32
        ));
    }
    let controls = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES,
    };
    if unsafe { InitCommonControlsEx(&controls) } == 0 {
        unsafe { CoUninitialize() };
        return Err("初始化原生列表控件失败".to_string());
    }

    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    if instance.is_null() {
        unsafe { CoUninitialize() };
        return Err("加载应用程序模块失败".to_string());
    }

    let mut app = Box::new(App::new(instance));
    APP.with(|slot| slot.set(&mut *app as *mut App));
    let outcome = run_window(instance);
    APP.with(|slot| slot.set(std::ptr::null_mut()));
    drop(app);
    unsafe { CoUninitialize() };
    outcome
}

unsafe fn set_dpi_awareness() {
    if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 {
        return;
    }
    // 清单已声明 PerMonitorV2 或系统不支持时，回退到系统级 DPI 感知。
    windows_sys::Win32::UI::WindowsAndMessaging::SetProcessDPIAware();
}

fn run_window(instance: HWND) -> Result<(), String> {
    let class_name = api::wide("OilFindNativeWindow");
    let cursor = unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW as usize as *const u16) };
    let brush = unsafe { GetSysColorBrush(COLOR_WINDOW) };
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        // CS_HREDRAW | CS_VREDRAW
        style: 0x0003,
        lpfnWndProc: Some(window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: std::ptr::null_mut(),
        hCursor: cursor,
        hbrBackground: brush,
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err("注册应用窗口失败".to_string());
    }

    let title = api::wide("Oil Find for Windows");
    let dpi = with_app(|app| app.dpi).unwrap_or(96);
    let mut width = 1120 * dpi / 96;
    let mut height = 760 * dpi / 96;
    let (screen_width, screen_height) =
        unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let available = screen_width - 32 * dpi / 96;
    if available > 0 && width > available {
        width = available;
    }
    let available = screen_height - 48 * dpi / 96;
    if available > 0 && height > available {
        height = available;
    }
    const CW_USEDEFAULT: i32 = -0x8000_0000;
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        let init_error = with_app(|app| app.init_error.clone()).flatten();
        return Err(init_error.unwrap_or_else(|| "创建应用窗口失败".to_string()));
    }

    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
    }
    // 窗口显示后再设置焦点：WM_CREATE 里设置的焦点会被随后的框架显示丢弃。
    with_app(|app| {
        if !app.search.is_null() {
            unsafe { SetFocus(app.search) };
        }
        app.start_search_worker();
        app.load_cache();
    });

    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
        if result == -1 {
            unsafe { DestroyWindow(hwnd) };
            break;
        }
        if result == 0 {
            break;
        }
        if with_app(|app| app.keyboard(&message)).unwrap_or(false) {
            continue;
        }
        if unsafe { IsDialogMessageW(hwnd, &message) } == 0 {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match with_app(|app| app.handle(hwnd, message, wparam, lparam)).flatten() {
        Some(result) => result,
        None => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

impl App {
    fn new(instance: HWND) -> Self {
        let (config_path, cache_path, startup_notice) = platform::config_paths();
        let dpi = unsafe { GetDpiForSystem() };
        let dpi = if (96..=480).contains(&dpi) {
            dpi as i32
        } else {
            96
        };
        let (events_tx, events_rx) = mpsc::channel();
        let mut app = App {
            hwnd: std::ptr::null_mut(),
            instance,
            font: std::ptr::null_mut(),
            dpi,
            search: std::ptr::null_mut(),
            roots_list: std::ptr::null_mut(),
            types: std::ptr::null_mut(),
            results: std::ptr::null_mut(),
            status: std::ptr::null_mut(),
            add: std::ptr::null_mut(),
            remove: std::ptr::null_mut(),
            start: std::ptr::null_mut(),
            stop: std::ptr::null_mut(),
            open: std::ptr::null_mut(),
            reveal: std::ptr::null_mut(),
            copy: std::ptr::null_mut(),
            labels: Vec::new(),
            config: Config::default(),
            config_path,
            cache_path,
            index: None,
            rows: Vec::new(),
            scan_cancel: None,
            scan_gate: Arc::new(Mutex::new(())),
            scan_generation: 0,
            search_generation: 0,
            scanning: false,
            auto_sync: false,
            closing: false,
            storage_notice: String::new(),
            last_notice: String::new(),
            startup_notice: startup_notice.unwrap_or_default(),
            init_error: None,
            last_update: None,
            last_progress: None,
            events_tx,
            events_rx,
            progress_slot: Arc::new(Mutex::new(None)),
            search_queue: Arc::new(SearchQueue {
                state: Mutex::new(SearchState {
                    request: None,
                    closed: false,
                }),
                ready: Condvar::new(),
            }),
        };
        app.load_configuration();
        app
    }

    fn handle(
        &mut self,
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match message {
            WM_CREATE => {
                self.hwnd = hwnd;
                match self.create_ui(hwnd) {
                    Ok(()) => Some(0),
                    Err(err) => {
                        self.init_error = Some(err);
                        Some(-1)
                    }
                }
            }
            WM_SIZE => {
                if !self.results.is_null() {
                    self.layout();
                }
                Some(0)
            }
            WM_DPICHANGED => {
                if !lparam_is_null(lparam) && !self.results.is_null() && !self.closing {
                    let suggested = unsafe { *(lparam as *const RECT) };
                    let dpi = (wparam & 0xFFFF) as i32;
                    if let Err(err) = self.change_dpi(dpi, suggested) {
                        self.last_notice = format!("调整显示缩放失败，保留现有界面：{err}");
                        self.render_status();
                    }
                }
                Some(0)
            }
            WM_GETMINMAXINFO => {
                if !lparam_is_null(lparam) {
                    let info = unsafe { &mut *(lparam as *mut MINMAXINFO) };
                    info.ptMinTrackSize.x = self.px(860);
                    info.ptMinTrackSize.y = self.px(620);
                }
                Some(0)
            }
            WM_COMMAND => {
                self.command(wparam & 0xFFFF, ((wparam >> 16) & 0xFFFF) as u32);
                Some(0)
            }
            WM_NOTIFY => {
                self.notify(lparam);
                Some(0)
            }
            WM_TIMER => {
                if wparam == TIMER_SEARCH {
                    unsafe { KillTimer(hwnd, TIMER_SEARCH) };
                    self.search_now();
                } else if wparam == TIMER_SYNC && self.auto_sync && !self.scanning {
                    self.begin_scan();
                }
                Some(0)
            }
            WM_APP_EVENT => {
                self.drain_events();
                Some(0)
            }
            WM_APP_PROGRESS => {
                self.show_progress();
                Some(0)
            }
            WM_CLOSE => {
                self.shutdown();
                unsafe { DestroyWindow(hwnd) };
                Some(0)
            }
            WM_DESTROY => {
                self.shutdown();
                unsafe { PostQuitMessage(0) };
                Some(0)
            }
            _ => None,
        }
    }

    fn create_ui(&mut self, hwnd: HWND) -> Result<(), String> {
        let window_dpi = unsafe { GetDpiForWindow(hwnd) };
        if (96..=480).contains(&window_dpi) {
            self.dpi = window_dpi as i32;
        }
        self.font = api::create_font(self.dpi)?;

        let make = |class: &str, label: &str, style: u32, ex_style: u32, id: usize| {
            api::create_control(
                hwnd,
                self.instance,
                class,
                label,
                style,
                ex_style,
                id,
                self.font,
            )
        };
        for label in [
            "搜索本机文件",
            "索引范围（仅本机目录；删除范围不会删除文件）",
            "文件类型",
            "索引只记录文件名与路径等元数据，不上传文件内容。",
        ] {
            let handle = make("STATIC", label, style::SS_LEFT, 0, 0)?;
            self.labels.push(handle);
        }
        self.search = make(
            "EDIT",
            "",
            style::WS_TABSTOP | style::ES_AUTOHSCROLL,
            style::WS_EX_CLIENTEDGE,
            ID_SEARCH,
        )?;
        api::send_message(self.search, style::EM_SETLIMITTEXT, 4096, 0);
        let cue = api::wide("中文 / 拼音 / 空格 AND / !排除 / OR 或 | / ext:pdf / size:>1mb");
        api::send_message(
            self.search,
            style::EM_SETCUEBANNER,
            0,
            cue.as_ptr() as isize,
        );
        for (label, id) in [
            ("添加目录…", ID_ADD_ROOT),
            ("删除范围", ID_REMOVE_ROOT),
            ("开始索引", ID_START),
            ("停止 / 暂停同步", ID_STOP),
            ("打开所选", ID_OPEN),
            ("资源管理器定位", ID_REVEAL),
            ("复制路径", ID_COPY),
        ] {
            let handle = make("BUTTON", label, style::WS_TABSTOP, 0, id)?;
            match id {
                ID_ADD_ROOT => self.add = handle,
                ID_REMOVE_ROOT => self.remove = handle,
                ID_START => self.start = handle,
                ID_STOP => self.stop = handle,
                ID_OPEN => self.open = handle,
                ID_REVEAL => self.reveal = handle,
                _ => self.copy = handle,
            }
        }
        self.roots_list = make(
            "LISTBOX",
            "",
            style::WS_TABSTOP | style::WS_VSCROLL | style::LBS_NOTIFY | style::LBS_NOINTEGRALHEIGHT,
            style::WS_EX_CLIENTEDGE,
            ID_ROOTS,
        )?;
        self.types = make(
            "COMBOBOX",
            "",
            style::WS_TABSTOP | style::WS_VSCROLL | style::CBS_DROPDOWNLIST,
            0,
            ID_TYPES,
        )?;
        for label in [
            "全部类型",
            "仅文件",
            "仅文件夹",
            "文档",
            "图片",
            "音频",
            "视频",
            "压缩包",
        ] {
            let buffer = api::wide(label);
            api::send_message(self.types, style::CB_ADDSTRING, 0, buffer.as_ptr() as isize);
        }
        api::send_message(self.types, style::CB_SETCURSEL, 0, 0);

        self.results = make(
            "SysListView32",
            "",
            style::WS_TABSTOP
                | style::WS_BORDER
                | style::LVS_REPORT
                | style::LVS_SINGLESEL
                | style::LVS_SHOWSELALWAYS,
            style::WS_EX_CLIENTEDGE,
            ID_RESULTS,
        )?;
        api::send_message(
            self.results,
            style::LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (style::LVS_EX_FULLROWSELECT | style::LVS_EX_GRIDLINES | style::LVS_EX_DOUBLEBUFFER)
                as isize,
        );
        for (position, label) in ["文件名", "完整路径", "大小", "修改日期"]
            .iter()
            .enumerate()
        {
            let text = api::wide(label);
            let column = LVCOLUMNW {
                mask: style::LVCF_FMT | style::LVCF_WIDTH | LVCF_TEXT | style::LVCF_SUBITEM,
                fmt: 0,
                cx: self.px(180),
                pszText: text.as_ptr() as *mut u16,
                iSubItem: position as i32,
                ..Default::default()
            };
            let result = api::send_message(
                self.results,
                style::LVM_INSERTCOLUMNW,
                position,
                &column as *const LVCOLUMNW as isize,
            );
            if result as i32 == -1 {
                return Err("创建结果列表列失败".to_string());
            }
        }
        self.status = make("STATIC", "", style::SS_LEFT, 0, 0)?;

        self.refresh_roots();
        self.selection_buttons();
        api::enable(self.stop, false);
        self.last_notice = "尚未启动索引。请先添加本机目录，再明确点击“开始索引”。".to_string();
        if !self.startup_notice.is_empty() {
            self.last_notice = self.startup_notice.clone();
        }
        self.render_status();
        self.layout();
        Ok(())
    }

    fn px(&self, value: i32) -> i32 {
        value * self.dpi / 96
    }

    fn layout(&mut self) {
        let mut area = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut area) } == 0 {
            return;
        }
        let width = area.right;
        let height = area.bottom;
        let padding = self.px(18);
        let gap = self.px(10);
        let row = self.px(32);

        let label_height = self.px(25);
        let label_gap = self.px(29);
        let mut y = padding + label_gap;
        let move_control = |handle: HWND, x: i32, y: i32, w: i32, h: i32| {
            if handle.is_null() {
                return;
            }
            unsafe {
                MoveWindow(handle, x, y, w.max(1), h.max(1), 1);
            }
        };
        move_control(
            self.labels[0],
            padding,
            padding,
            width - 2 * padding,
            label_height,
        );
        move_control(self.search, padding, y, width - 2 * padding, row);
        y += row + gap;
        move_control(
            self.labels[1],
            padding,
            y,
            width - 2 * padding,
            label_height,
        );
        y += label_gap;

        let buttons_width = self.px(145);
        move_control(
            self.roots_list,
            padding,
            y,
            width - 3 * padding - buttons_width,
            self.px(100),
        );
        move_control(
            self.add,
            width - padding - buttons_width,
            y,
            buttons_width,
            row,
        );
        move_control(
            self.remove,
            width - padding - buttons_width,
            y + row + gap,
            buttons_width,
            row,
        );
        y += self.px(100) + gap;

        move_control(self.start, padding, y, self.px(125), row);
        move_control(self.stop, padding + self.px(135), y, self.px(165), row);
        move_control(
            self.labels[2],
            width - padding - self.px(280),
            y + self.px(4),
            self.px(75),
            row,
        );
        move_control(
            self.types,
            width - padding - self.px(195),
            y,
            self.px(195),
            self.px(240),
        );
        y += row + gap;

        let bottom = height - self.px(112);
        move_control(self.results, padding, y, width - 2 * padding, bottom - y);
        move_control(self.open, padding, bottom + gap, self.px(115), row);
        move_control(
            self.reveal,
            padding + self.px(125),
            bottom + gap,
            self.px(165),
            row,
        );
        move_control(
            self.copy,
            padding + self.px(300),
            bottom + gap,
            self.px(115),
            row,
        );
        move_control(
            self.status,
            padding,
            height - self.px(63),
            width - 2 * padding,
            self.px(29),
        );
        move_control(
            self.labels[3],
            padding,
            height - self.px(30),
            width - 2 * padding,
            self.px(24),
        );

        for (position, mut column_width) in [
            self.px(220),
            width - 2 * padding - self.px(505),
            self.px(100),
            self.px(165),
        ]
        .into_iter()
        .enumerate()
        {
            if column_width < self.px(100) {
                column_width = self.px(100);
            }
            api::send_message(
                self.results,
                style::LVM_SETCOLUMNWIDTH,
                position,
                column_width as isize,
            );
        }
    }

    fn change_dpi(&mut self, dpi: i32, suggested: RECT) -> Result<(), String> {
        if !(96..=768).contains(&dpi)
            || suggested.right <= suggested.left
            || suggested.bottom <= suggested.top
        {
            return Err("显示缩放参数无效".to_string());
        }
        let font = api::create_font(dpi)?;
        let previous = self.font;
        self.font = font;
        self.dpi = dpi;
        let mut controls = self.labels.clone();
        controls.extend([
            self.search,
            self.roots_list,
            self.types,
            self.results,
            self.status,
            self.add,
            self.remove,
            self.start,
            self.stop,
            self.open,
            self.reveal,
            self.copy,
        ]);
        for handle in controls {
            api::set_font(handle, font);
        }
        // 按系统建议矩形重建窗口；随后的 WM_SIZE 仍会重新布局。
        unsafe {
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.layout();
        api::delete_font(previous);
        Ok(())
    }

    fn keyboard(&mut self, message: &MSG) -> bool {
        if message.message != WM_KEYDOWN {
            return false;
        }
        let control = unsafe { GetKeyState(0x11) };
        if message.wParam == 'L' as usize && control < 0 {
            unsafe { SetFocus(self.search) };
            api::send_message(self.search, style::EM_SETSEL, 0, -1);
            return true;
        }
        if message.wParam == 0x1B {
            // Esc：清空搜索并丢弃当前结果。
            api::put_text(self.search, "");
            unsafe { KillTimer(self.hwnd, TIMER_SEARCH) };
            self.search_generation += 1;
            self.set_rows(Vec::new());
            self.last_notice = "已取消搜索；索引与文件未改动。".to_string();
            self.render_status();
            return true;
        }
        if message.wParam == 0x0D {
            let focus = unsafe { GetFocus() };
            if focus == self.results {
                self.file_action(ID_OPEN);
                return true;
            }
            if focus == self.search {
                unsafe { KillTimer(self.hwnd, TIMER_SEARCH) };
                self.search_now();
                unsafe { SetFocus(self.results) };
                return true;
            }
        }
        false
    }

    fn command(&mut self, id: usize, notification: u32) {
        if self.closing {
            return;
        }
        match id {
            ID_SEARCH => {
                // EN_CHANGE
                if notification == 0x0300 && !self.search.is_null() {
                    self.search_generation += 1;
                    unsafe { SetTimer(self.hwnd, TIMER_SEARCH, SEARCH_DEBOUNCE_MS, None) };
                }
            }
            ID_TYPES => {
                // CBN_SELCHANGE
                if notification == 1 {
                    self.search_generation += 1;
                    self.search_now();
                }
            }
            ID_ADD_ROOT if notification == 0 => self.add_root(),
            ID_REMOVE_ROOT if notification == 0 => self.remove_root(),
            ID_START if notification == 0 => {
                if self.config.roots.is_empty() {
                    api::warn(
                        self.hwnd,
                        "请先添加至少一个本机目录；应用不会自动选择或扫描个人目录。",
                    );
                    return;
                }
                self.auto_sync = true;
                unsafe { SetTimer(self.hwnd, TIMER_SYNC, SYNC_INTERVAL_MS, None) };
                self.begin_scan();
            }
            ID_STOP if notification == 0 => {
                self.pause_scan();
                self.last_notice = "已停止扫描并暂停 30 秒同步；已有索引仍可搜索。".to_string();
                self.render_status();
            }
            ID_ROOTS => {
                // LBN_SELCHANGE
                if notification == 1 {
                    let selected = api::send_message(self.roots_list, style::LB_GETCURSEL, 0, 0);
                    api::enable(self.remove, selected >= 0);
                }
            }
            ID_OPEN | ID_REVEAL | ID_COPY if notification == 0 => self.file_action(id),
            _ => {}
        }
    }

    fn notify(&mut self, lparam: LPARAM) {
        if lparam_is_null(lparam) {
            return;
        }
        let header = unsafe { &*(lparam as *const NMHDR) };
        if header.hwndFrom != self.results {
            return;
        }
        let code = header.code as i32;
        if code == NM_DBLCLK as i32 {
            let activation = unsafe { &*(lparam as *const NMITEMACTIVATE) };
            if activation.iItem >= 0 && (activation.iItem as usize) < self.rows.len() {
                self.file_action(ID_OPEN);
            }
        } else if code == LVN_ITEMCHANGED as i32 {
            self.selection_buttons();
        }
    }

    fn add_root(&mut self) {
        let path = match api::choose_folder(self.hwnd) {
            Ok(path) => path,
            Err(err) => {
                api::warn(self.hwnd, &err);
                return;
            }
        };
        if path.is_empty() {
            return;
        }
        let path = match platform::local_directory(Path::new(&path)) {
            Ok(path) => path,
            Err(err) => {
                api::warn(self.hwnd, &err);
                return;
            }
        };
        let path = path.to_string_lossy().to_string();
        if self
            .config
            .roots
            .iter()
            .any(|root| platform::within_root(Path::new(root), Path::new(&path)))
        {
            api::warn(self.hwnd, "该目录已包含在现有索引范围内。");
            return;
        }
        if self.config.roots.len() >= MAX_ROOTS {
            api::warn(self.hwnd, &format!("最多可配置 {MAX_ROOTS} 个索引目录。"));
            return;
        }
        self.invalidate_scope();
        // 新目录覆盖旧范围时，旧的子目录配置被合并掉。
        let mut roots: Vec<String> = self
            .config
            .roots
            .iter()
            .filter(|root| !platform::within_root(Path::new(&path), Path::new(root)))
            .cloned()
            .collect();
        roots.push(path);
        self.config.roots = roots;
        self.scope_changed("已添加本机目录。请点击“开始索引”；尚未自动扫描。");
    }

    fn remove_root(&mut self) {
        let selected = api::send_message(self.roots_list, style::LB_GETCURSEL, 0, 0);
        if selected < 0 || selected as usize >= self.config.roots.len() {
            return;
        }
        self.invalidate_scope();
        self.config.roots.remove(selected as usize);
        self.scope_changed("已删除索引范围配置，未删除任何个人文件。重新索引前不显示旧范围结果。");
    }

    fn invalidate_scope(&mut self) {
        self.pause_scan();
        self.search_generation += 1;
        self.index = None;
        self.config.cache_roots.clear();
        self.last_update = None;
        self.set_rows(Vec::new());
    }

    fn scope_changed(&mut self, notice: &str) {
        self.last_notice = notice.to_string();
        if let Err(err) = self.save_configuration() {
            self.last_notice = format!("{} 配置未保存：{err}", self.last_notice);
        }
        self.refresh_roots();
        self.render_status();
    }

    fn refresh_roots(&mut self) {
        api::send_message(self.roots_list, style::LB_RESETCONTENT, 0, 0);
        for root in &self.config.roots {
            let buffer = api::wide(root);
            api::send_message(
                self.roots_list,
                style::LB_ADDSTRING,
                0,
                buffer.as_ptr() as isize,
            );
        }
        api::enable(self.start, !self.config.roots.is_empty() && !self.scanning);
        api::enable(self.remove, false);
    }

    fn begin_scan(&mut self) {
        if self.scanning || self.config.roots.is_empty() {
            return;
        }
        self.scan_generation += 1;
        let generation = self.scan_generation;
        let roots: Vec<PathBuf> = self.config.roots.iter().map(PathBuf::from).collect();
        let cancel = Arc::new(AtomicBool::new(false));
        self.scan_cancel = Some(Arc::clone(&cancel));
        self.scanning = true;
        api::enable(self.start, false);
        api::enable(self.stop, true);
        self.last_notice = "正在建立本机元数据快照；界面仍可操作。".to_string();
        self.render_status();

        let sender = self.events_tx.clone();
        let hwnd = SendHwnd(self.hwnd);
        let cache_path = self.cache_path.clone();
        let gate = Arc::clone(&self.scan_gate);
        let progress_slot = Arc::clone(&self.progress_slot);
        std::thread::spawn(move || {
            // 同一时刻只允许一个扫描线程写缓存；被取消的旧任务会很快释放。
            let _guard = lock(&gate);
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            for root in &roots {
                if let Err(err) = platform::local_directory(root) {
                    send_event(
                        &sender,
                        hwnd,
                        Event::Scan {
                            generation,
                            outcome: Err(err),
                        },
                    );
                    return;
                }
            }
            let last_progress = Mutex::new(Instant::now() - PROGRESS_INTERVAL);
            let scan_result = scan(&cancel, &roots, |progress| {
                {
                    let mut last = lock(&last_progress);
                    if last.elapsed() < PROGRESS_INTERVAL {
                        return;
                    }
                    *last = Instant::now();
                }
                *lock(&progress_slot) = Some(progress);
                hwnd.post(WM_APP_PROGRESS);
            });
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            let index = match scan_result {
                Ok(index) => index,
                Err(err) => {
                    send_event(
                        &sender,
                        hwnd,
                        Event::Scan {
                            generation,
                            outcome: Err(err.to_string()),
                        },
                    );
                    return;
                }
            };
            let mut save_error = None;
            if let Some(path) = cache_path {
                if let Err(err) = index.save_cancellable(&cancel, &path) {
                    save_error = Some(err.to_string());
                }
                // 保存返回后若任务已被取消，该快照不再可信：直接丢弃。
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
            }
            send_event(
                &sender,
                hwnd,
                Event::Scan {
                    generation,
                    outcome: Ok(ScanSuccess {
                        index,
                        save_error,
                        updated: SystemTime::now(),
                    }),
                },
            );
        });
    }

    fn pause_scan(&mut self) {
        self.auto_sync = false;
        unsafe { KillTimer(self.hwnd, TIMER_SYNC) };
        self.scan_generation += 1;
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        self.scanning = false;
        api::enable(self.start, !self.config.roots.is_empty());
        api::enable(self.stop, false);
    }

    fn start_search_worker(&mut self) {
        let queue = Arc::clone(&self.search_queue);
        let sender = self.events_tx.clone();
        let hwnd = SendHwnd(self.hwnd);
        std::thread::spawn(move || loop {
            let request = {
                let mut state = lock(&queue.state);
                while state.request.is_none() && !state.closed {
                    state = match queue.ready.wait(state) {
                        Ok(guard) => guard,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                }
                if state.closed {
                    return;
                }
                state.request.take()
            };
            let Some(request) = request else { continue };
            let outcome = request
                .index
                .search(&request.query, request.kind.as_deref(), RESULT_LIMIT)
                .map_err(|err| err.to_string());
            if sender
                .send(Event::Search {
                    generation: request.generation,
                    outcome,
                })
                .is_err()
            {
                return;
            }
            hwnd.post(WM_APP_EVENT);
        });
    }

    fn search_now(&mut self) {
        self.search_generation += 1;
        let Some(index) = self.index.clone() else {
            self.set_rows(Vec::new());
            self.last_notice = "还没有可用索引。添加目录后点击“开始索引”。".to_string();
            self.render_status();
            return;
        };
        let query = api::text(self.search).trim().to_string();
        let selected = api::send_message(self.types, style::CB_GETCURSEL, 0, 0);
        let kinds = [
            None,
            Some("file"),
            Some("folder"),
            Some("doc"),
            Some("image"),
            Some("audio"),
            Some("video"),
            Some("archive"),
        ];
        let kind = kinds
            .get(selected.max(0) as usize)
            .copied()
            .flatten()
            .map(str::to_string);
        self.last_notice = "正在搜索已有索引…".to_string();
        self.render_status();
        let request = SearchRequest {
            generation: self.search_generation,
            index,
            query,
            kind,
        };
        // 信箱容量为 1：直接替换尚未执行的请求，UI 线程不会阻塞。
        let mut state = lock(&self.search_queue.state);
        state.request = Some(request);
        drop(state);
        self.search_queue.ready.notify_one();
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.events_rx.try_recv() {
            self.handle_event(event);
        }
    }

    fn show_progress(&mut self) {
        let progress = lock(&self.progress_slot).take();
        if let Some(progress) = progress {
            self.last_progress = Some(progress);
        }
        let Some(progress) = self.last_progress.as_ref() else {
            return;
        };
        if self.closing || !self.scanning {
            return;
        }
        self.last_notice = format!(
            "扫描中：已访问 {} 项，跳过 {} 项 · {}",
            progress.visited,
            progress.skipped,
            progress.current.display()
        );
        self.render_status();
    }

    fn handle_event(&mut self, event: Event) {
        if self.closing {
            return;
        }
        match event {
            Event::Scan {
                generation,
                outcome,
            } => {
                if generation != self.scan_generation {
                    return;
                }
                self.scanning = false;
                self.scan_cancel = None;
                api::enable(self.start, !self.config.roots.is_empty());
                api::enable(self.stop, self.auto_sync);
                let success = match outcome {
                    Ok(success) => success,
                    Err(err) => {
                        // 失败就停掉 30 秒定时器：反复重试失败任务只会白烧 CPU。
                        self.auto_sync = false;
                        unsafe { KillTimer(self.hwnd, TIMER_SYNC) };
                        api::enable(self.stop, false);
                        self.last_notice = format!(
                            "索引失败，已有缓存仍可使用；自动同步已暂停，请处理后再手动开始：{err}"
                        );
                        self.render_status();
                        return;
                    }
                };
                // 缓存未持久化时不能替换当前索引：旧索引与旧搜索结果继续可用。
                if self.cache_path.is_some() {
                    if let Some(err) = success.save_error {
                        self.last_notice = "本次索引未提交，保留原索引与搜索结果。".to_string();
                        self.storage_notice = format!("缓存未保存：{err}");
                        self.render_status();
                        return;
                    }
                }
                let count = success.index.len();
                self.index = Some(Arc::new(success.index));
                self.last_update = Some(success.updated);
                self.storage_notice.clear();
                self.last_notice = format!("索引完成：{count} 项。");
                if self.cache_path.is_none() {
                    // 启动时已明确降级为仅内存模式：快照可用，但没有有效缓存范围。
                    self.storage_notice =
                        "仅内存索引，退出后丢失；配置与缓存未持久化。".to_string();
                    self.config.cache_roots.clear();
                } else {
                    // 只有真正持久化成功，cache_roots 才代表下次启动可加载的范围。
                    self.config.cache_roots = self.config.roots.clone();
                    if let Err(err) = self.save_configuration() {
                        self.config.cache_roots.clear();
                        self.storage_notice = format!("配置未保存：{err}");
                    }
                }
                self.render_status();
                self.search_now();
            }
            Event::Cache {
                generation,
                outcome,
            } => {
                if generation != self.scan_generation || self.index.is_some() {
                    return;
                }
                match outcome {
                    Ok((index, updated)) => {
                        let count = index.len();
                        self.index = Some(Arc::new(index));
                        self.last_update = Some(updated);
                        self.last_notice = format!(
                            "已加载本机缓存（{count} 项），未启动扫描。点击“开始索引”才启用同步。"
                        );
                        self.render_status();
                        self.search_now();
                    }
                    Err(err) => {
                        self.last_notice = format!("未加载索引缓存：{err}。请手动开始索引。");
                        self.render_status();
                    }
                }
            }
            Event::Search {
                generation,
                outcome,
            } => {
                if generation != self.search_generation {
                    return;
                }
                match outcome {
                    Ok((entries, total)) => {
                        self.set_rows(entries);
                        self.last_notice =
                            format!("共命中 {total} 项，显示 {} 项", self.rows.len());
                        if total > RESULT_LIMIT {
                            self.last_notice
                                .push_str(&format!("（最多 {RESULT_LIMIT} 项，请缩小搜索条件）"));
                        }
                    }
                    Err(err) => {
                        self.set_rows(Vec::new());
                        self.last_notice = format!("搜索条件有误：{err}");
                    }
                }
                self.render_status();
            }
        }
    }

    fn set_rows(&mut self, entries: Vec<Entry>) {
        api::send_message(self.results, style::WM_SETREDRAW, 0, 0);
        api::send_message(self.results, style::LVM_DELETEALLITEMS, 0, 0);
        self.rows = entries;
        for (position, entry) in self.rows.iter().enumerate() {
            let name = api::wide(&entry.name);
            let mut item = LVITEMW {
                mask: LVIF_TEXT,
                iItem: position as i32,
                pszText: name.as_ptr() as *mut u16,
                ..Default::default()
            };
            api::send_message(
                self.results,
                style::LVM_INSERTITEMW,
                0,
                &item as *const LVITEMW as isize,
            );
            let size = if entry.is_dir {
                "文件夹".to_string()
            } else {
                platform::format_size(entry.size)
            };
            let values = [
                entry.path.to_string_lossy().to_string(),
                size,
                platform::format_timestamp(entry.modified, false),
            ];
            for (offset, value) in values.iter().enumerate() {
                let buffer = api::wide(value);
                item.iSubItem = offset as i32 + 1;
                item.pszText = buffer.as_ptr() as *mut u16;
                api::send_message(
                    self.results,
                    style::LVM_SETITEMW,
                    0,
                    &item as *const LVITEMW as isize,
                );
            }
        }
        api::send_message(self.results, style::WM_SETREDRAW, 1, 0);
        unsafe {
            InvalidateRect(self.results, std::ptr::null(), 1);
        }
        self.selection_buttons();
    }

    fn selected_entry(&self) -> Option<Entry> {
        let selected = api::send_message(
            self.results,
            style::LVM_GETNEXTITEM,
            usize::MAX,
            style::LVNI_SELECTED as isize,
        );
        if selected < 0 || selected as usize >= self.rows.len() {
            return None;
        }
        Some(self.rows[selected as usize].clone())
    }

    fn selection_buttons(&mut self) {
        let selected = self.selected_entry().is_some();
        api::enable(self.open, selected);
        api::enable(self.reveal, selected);
        api::enable(self.copy, selected);
    }

    fn file_action(&mut self, action: usize) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let path = entry.path.clone();
        if let Err(err) = platform::local_path(&path) {
            api::warn(self.hwnd, &err);
            return;
        }
        if !self.in_scope(&path) {
            api::warn(self.hwnd, "该项目已不在当前索引范围内，请重新索引。");
            return;
        }
        if std::fs::symlink_metadata(&path).is_err() {
            api::warn(self.hwnd, "所选项目不存在或无法访问，请重新索引。");
            return;
        }
        if platform::is_link_or_reparse(&path) {
            api::warn(
                self.hwnd,
                "为避免打开范围外目标，不打开链接或重解析点。请直接选择本机目标目录建立索引。",
            );
            return;
        }
        let resolved = match platform::resolve(&path) {
            Ok(resolved) => resolved,
            Err(err) => {
                api::warn(self.hwnd, &format!("无法验证所选项目的本机路径：{err}"));
                return;
            }
        };
        if let Err(err) = platform::local_path(&resolved) {
            api::warn(self.hwnd, &err);
            return;
        }
        if !self.in_scope(&resolved) {
            api::warn(
                self.hwnd,
                "项目实际位置已不在当前本机索引范围内，请重新索引。",
            );
            return;
        }
        let outcome = match action {
            ID_OPEN => api::open_path(self.hwnd, &resolved),
            ID_REVEAL => api::reveal_path(&resolved),
            _ => api::copy_to_clipboard(self.hwnd, &resolved.to_string_lossy()),
        };
        if let Err(err) = outcome {
            api::warn(self.hwnd, &err);
        }
    }

    fn in_scope(&self, path: &Path) -> bool {
        self.config
            .roots
            .iter()
            .any(|root| platform::within_root(Path::new(root), path))
    }

    fn render_status(&mut self) {
        let mut mode = "同步未启动".to_string();
        if self.auto_sync {
            mode = "30 秒同步已启用".to_string();
        }
        if self.scanning {
            mode = format!("扫描中 · {mode}");
        }
        if self.cache_path.is_none() {
            mode = format!("仅内存 · 不持久化 · {mode}");
        }
        let count = self.index.as_ref().map_or(0, |index| index.len());
        let updated = match self.last_update {
            Some(updated) => format!(" · 快照 {}", system_time_label(updated)),
            None => String::new(),
        };
        let notice =
            format!("{} {}", self.last_notice, self.storage_notice).replace(['\r', '\n'], " ");
        let text = format!("{mode} · 索引 {count} 项{updated} · {notice}");
        let buffer = api::wide(&text);
        unsafe {
            SetWindowTextW(self.status, buffer.as_ptr());
        }
    }

    fn load_configuration(&mut self) {
        let Some(config_path) = self.config_path.clone() else {
            return;
        };
        if platform::is_link_or_reparse(&config_path) {
            self.startup_notice = "目录配置是链接，未加载；请重新添加本机目录。".to_string();
            return;
        }
        let data = match std::fs::read(&config_path) {
            Ok(data) => data,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(err) => {
                self.startup_notice = format!("读取目录配置失败；请重新添加本机目录：{err}");
                return;
            }
        };
        if data.len() > 1024 * 1024 {
            self.startup_notice = "目录配置超出安全上限；请重新添加目录。".to_string();
            return;
        }
        let config: Config = match serde_json::from_slice(&data) {
            Ok(config) => config,
            Err(_) => {
                self.startup_notice = "目录配置损坏；不会自动扫描，请重新添加目录。".to_string();
                return;
            }
        };
        if config.roots.len() > MAX_ROOTS {
            self.startup_notice = "目录配置超出安全上限；请重新添加目录。".to_string();
            return;
        }
        let mut roots: Vec<String> = Vec::new();
        for root in &config.roots {
            match platform::local_directory(Path::new(root)) {
                Ok(path) => {
                    let path = path.to_string_lossy().to_string();
                    let duplicate = roots
                        .iter()
                        .any(|existing| existing.eq_ignore_ascii_case(&path));
                    if !duplicate {
                        roots.push(path);
                    }
                }
                Err(_) => {
                    self.startup_notice =
                        "已跳过无法访问或非本机的已保存目录；不会自动扫描。".to_string();
                }
            }
        }
        // 顺序无关：手工调整过顺序的配置不应让仍然有效的缓存失效。
        if platform::same_root_set(&config.roots, &roots)
            && platform::same_root_set(&config.cache_roots, &roots)
        {
            self.config.cache_roots = roots.clone();
        }
        self.config.roots = roots;
    }

    fn save_configuration(&self) -> Result<(), String> {
        let Some(config_path) = self.config_path.as_ref() else {
            return Err("当前用户配置目录不可用".to_string());
        };
        let data = serde_json::to_vec_pretty(&self.config).map_err(|err| err.to_string())?;
        let parent = config_path
            .parent()
            .ok_or_else(|| "配置路径无效".to_string())?;
        let temporary = parent.join(format!("config-{}.tmp", std::process::id()));
        std::fs::write(&temporary, data).map_err(|err| err.to_string())?;
        std::fs::rename(&temporary, config_path).map_err(|err| {
            let _ = std::fs::remove_file(&temporary);
            err.to_string()
        })
    }

    fn load_cache(&mut self) {
        let (Some(cache_path), false) = (self.cache_path.clone(), self.config.roots.is_empty())
        else {
            return;
        };
        if !platform::same_root_set(&self.config.roots, &self.config.cache_roots) {
            return;
        }
        let generation = self.scan_generation;
        let sender = self.events_tx.clone();
        let hwnd = SendHwnd(self.hwnd);
        let roots: Vec<PathBuf> = self.config.roots.iter().map(PathBuf::from).collect();
        std::thread::spawn(move || {
            let outcome = (|| -> Result<(Index, SystemTime), String> {
                let metadata = std::fs::symlink_metadata(&cache_path)
                    .map_err(|err| format!("缓存不可用：{err}"))?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err("索引缓存不是普通本机文件".to_string());
                }
                let index = Index::load(&cache_path).map_err(|err| format!("缓存损坏：{err}"))?;
                if !platform::same_root_paths(&index.roots, &roots) {
                    return Err("缓存目录与当前索引范围不一致".to_string());
                }
                let updated = metadata.modified().unwrap_or_else(|_| SystemTime::now());
                Ok((index, updated))
            })();
            send_event(
                &sender,
                hwnd,
                Event::Cache {
                    generation,
                    outcome,
                },
            );
        });
    }

    fn shutdown(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        unsafe {
            KillTimer(self.hwnd, TIMER_SEARCH);
            KillTimer(self.hwnd, TIMER_SYNC);
        }
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        let mut state = lock(&self.search_queue.state);
        state.closed = true;
        state.request = None;
        drop(state);
        self.search_queue.ready.notify_all();
    }
}

impl Drop for App {
    fn drop(&mut self) {
        api::delete_font(self.font);
        self.font = std::ptr::null_mut();
    }
}

fn send_event(sender: &Sender<Event>, hwnd: SendHwnd, event: Event) {
    if sender.send(event).is_ok() {
        hwnd.post(WM_APP_EVENT);
    }
}

fn lparam_is_null(lparam: LPARAM) -> bool {
    lparam == 0
}

/// 状态栏里的「快照 MM-DD HH:MM:SS」。
fn system_time_label(time: SystemTime) -> String {
    let seconds = match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs() as i64,
        Err(_) => 0,
    };
    let text = platform::format_timestamp(seconds, true);
    match text.split_once(' ') {
        Some((date, clock)) => month_day_label(date, clock),
        None => text,
    }
}

/// "YYYY-MM-DD" + "HH:MM:SS" -> "MM-DD HH:MM:SS"（状态栏不显示年份）。
fn month_day_label(date: &str, clock: &str) -> String {
    let mut parts = date.split('-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(_), Some(month), Some(day)) => format!("{month}-{day} {clock}"),
        _ => format!("{date} {clock}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_label_drops_the_year() {
        assert_eq!(month_day_label("2024-01-05", "08:09:10"), "01-05 08:09:10");
        // 无法解析的日期原样保留，不吞掉信息。
        assert_eq!(month_day_label("bad", "08:09:10"), "bad 08:09:10");
    }
}
