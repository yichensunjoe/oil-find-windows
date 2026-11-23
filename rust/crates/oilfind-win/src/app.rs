//! 主窗口：原生 Win32 界面、后台扫描/搜索线程、事件分发。
//!
//! 线程模型：
//! - UI 线程只做界面更新，**从不**等待后台任务（无 join、无阻塞 recv）。
//! - 扫描线程与搜索线程通过 `mpsc` 送回完成事件，再用 `PostMessageW` 唤醒 UI。
//! - 进度通过 `PostMessage(WM_APP + 2)` 通知，UI 线程读取最新的进度快照。

use std::cell::{Cell, RefCell};
use std::cmp;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use oilfind_core::{compare_entries, scan, Entry, Index, Progress};
use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
// ClientToScreen / ScreenToClient 在 Gdi 模块（user32 转发），与绘制用同一组。
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, FillRect, GetStockObject, GetSysColorBrush, InvalidateRect, ScreenToClient,
    SetBkMode, SetTextColor, UpdateWindow, BLACK_BRUSH, COLOR_WINDOW, HDC, HFONT, TRANSPARENT,
};
use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
// 表头排序、命中测试与进度条类所需的常量与结构。
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, HDITEMW, HDI_TEXT, HDN_ITEMCLICK, ICC_LISTVIEW_CLASSES,
    ICC_PROGRESS_CLASS, INITCOMMONCONTROLSEX, LVCF_TEXT, LVCOLUMNW, LVHITTESTINFO, LVIF_TEXT,
    LVITEMW, LVN_ITEMCHANGED, NMHDR, NMHEADERW, NMITEMACTIVATE, NM_DBLCLK,
};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, RegisterHotKey, SetFocus, UnregisterHotKey, MOD_ALT, MOD_CONTROL,
    MOD_NOREPEAT, VK_SPACE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetClientRect, GetForegroundWindow, GetMessageW, IsDialogMessageW, IsIconic,
    KillTimer, LoadCursorW, MoveWindow, PostQuitMessage, RegisterClassExW, SetForegroundWindow,
    SetTimer, SetWindowPos, SetWindowTextW, ShowWindow, TrackPopupMenu, TranslateMessage,
    IDC_ARROW, MF_STRING, MINMAXINFO, MSG, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_MINIMIZE,
    SW_RESTORE, SW_SHOW, TPM_LEFTALIGN, TPM_RETURNCMD, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CONTEXTMENU, WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO,
    WM_HOTKEY, WM_KEYDOWN, WM_NOTIFY, WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_BORDER, WS_CLIPCHILDREN,
    WS_EX_APPWINDOW, WS_EX_CONTROLPARENT, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
};

use crate::api::{self, SendHwnd};
use crate::platform;
use crate::quick_panel::{self, Panel};

const RESULT_LIMIT: usize = 2000;
const MAX_ROOTS: usize = 64;
const SEARCH_DEBOUNCE_MS: u32 = 250;
const REFRESH_PRESETS: [(u32, &str); 7] = [
    (0, "仅手动"),
    (15, "每 15 分钟"),
    (30, "每 30 分钟"),
    (60, "每小时"),
    (120, "每 2 小时"),
    (360, "每 6 小时"),
    (1440, "每天"),
];
const DEFAULT_REFRESH_PRESET: usize = 3;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);

fn progress_control_class() -> &'static str {
    use windows_sys::Win32::UI::Controls::PROGRESS_CLASSA;

    // SAFETY: the SDK constant points to a static NUL-terminated ASCII name.
    unsafe { std::ffi::CStr::from_ptr(PROGRESS_CLASSA.cast()) }
        .to_str()
        .expect("the SDK's progress control class is ASCII")
}

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
const SEARCH_HOTKEY: i32 = 1;
const ID_SETTINGS: usize = 150;
const ID_HIDE: usize = 151;
const ID_FIXED_ROOTS: usize = 152;
const ID_REFRESH_INTERVAL: usize = 153;

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
    // No owner-data or owner-draw: the native control owns its item text.
    pub const RESULTS_STYLE: u32 =
        WS_TABSTOP | WS_BORDER | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS;
    pub const LVS_EX_FULLROWSELECT: u32 = 0x0000_0020;
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
    pub const CB_GETDROPPEDSTATE: u32 = 0x0157;
    pub const CB_GETCURSEL: u32 = 0x0147;
    pub const LB_ADDSTRING: u32 = 0x0180;
    pub const LB_RESETCONTENT: u32 = 0x0184;
    pub const LB_GETCURSEL: u32 = 0x0188;
    pub const EM_SETLIMITTEXT: u32 = 0x00C5;
    pub const EM_SETCUEBANNER: u32 = 0x1501;
    pub const EM_SETSEL: u32 = 0x00B1;
    pub const WM_SETREDRAW: u32 = 0x000B;
    // 结果列表：表头、命中测试、选中状态。
    pub const LVM_GETHEADER: u32 = LVM_FIRST + 31;
    pub const LVM_HITTEST: u32 = LVM_FIRST + 18;
    pub const LVM_SETITEMSTATE: u32 = LVM_FIRST + 43;
    pub const LVIS_SELECTED: u32 = 0x0002;
    // 表头消息（HDM_FIRST = 0x1200）与排序点击通知。
    pub const HDM_FIRST: u32 = 0x1200;
    pub const HDM_SETITEMW: u32 = HDM_FIRST + 12;
    // 进度条：marquee 动画（PBM_SETMARQUEE = WM_USER + 10）。
    pub const PBS_MARQUEE: u32 = 0x0000_0008;
    pub const PBM_SETMARQUEE: u32 = 0x0400 + 10;
    // 静态控件：居中文本；默认按钮（“开始索引”）。
    pub const SS_CENTER: u32 = 0x0000_0001;
    pub const EM_SETMARGINS: u32 = 0x00D3;
    // 静态控件颜色通知：wParam 是 HDC，lParam 是控件句柄。
    pub const WM_CTLCOLORSTATIC: u32 = 0x0138;
}

// Win32 may call the window procedure synchronously from almost any control
// operation, including modal dialogs. Never create a second mutable App borrow:
// copy deferrable messages into this UI-thread queue and replay them after the
// current borrow ends. Messages needing synchronous results are handled directly
// in window_proc, using the separate read-only window snapshot below.
thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static PENDING: RefCell<VecDeque<Deferred>> = const { RefCell::new(VecDeque::new()) };
    static DRAINING: Cell<bool> = const { Cell::new(false) };
    static WINDOW_STATE: Cell<WindowState> = const { Cell::new(WindowState::Starting) };
    static WINDOW_DPI: Cell<i32> = const { Cell::new(96) };
    static WINDOW_RESULTS: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static WINDOW_HEADER: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static WINDOW_EMPTY: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static WINDOW_GLASS: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowState {
    Starting,
    Running,
    Stopped,
}

fn mark_window_stopped(state: &Cell<WindowState>) -> bool {
    state.replace(WindowState::Stopped) == WindowState::Running
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Notification {
    HeaderClicked(i32),
    ResultDoubleClicked(i32),
    ResultSelectionChanged,
}

#[derive(Clone, Copy)]
enum Deferred {
    Size,
    DpiChanged { dpi: i32, suggested: RECT },
    Command { id: usize, notification: u32 },
    ContextMenu { control: HWND, position: LPARAM },
    Notify(Notification),
    Timer(usize),
    Events,
    Progress,
    ToggleSearch,
    Close,
    Destroyed,
}

impl Deferred {
    fn coalesces_with(self, other: Self) -> bool {
        match (self, other) {
            (Self::Size, Self::Size)
            | (Self::Events, Self::Events)
            | (Self::Progress, Self::Progress)
            | (
                Self::Notify(Notification::ResultSelectionChanged),
                Self::Notify(Notification::ResultSelectionChanged),
            ) => true,
            (Self::Timer(left), Self::Timer(right)) => left == right,
            _ => false,
        }
    }
}

enum AppAccess<R> {
    Ready(R),
    Busy,
    Missing,
}

fn try_with_state<T, R>(slot: &RefCell<Option<T>>, f: impl FnOnce(&mut T) -> R) -> AppAccess<R> {
    let Ok(mut borrowed) = slot.try_borrow_mut() else {
        return AppAccess::Busy;
    };
    match borrowed.as_mut() {
        Some(state) => AppAccess::Ready(f(state)),
        None => AppAccess::Missing,
    }
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    match APP.with(|slot| try_with_state(slot, f)) {
        AppAccess::Ready(result) => {
            drain_pending();
            Some(result)
        }
        AppAccess::Busy | AppAccess::Missing => None,
    }
}

fn push_deferred(pending: &mut VecDeque<Deferred>, message: Deferred) {
    if !pending
        .iter()
        .any(|existing| message.coalesces_with(*existing))
    {
        pending.push_back(message);
    }
}

fn queue_deferred(message: Deferred) {
    PENDING.with(|pending| push_deferred(&mut pending.borrow_mut(), message));
}

fn dispatch_deferred(message: Deferred) {
    if WINDOW_STATE.with(Cell::get) == WindowState::Stopped
        && !matches!(message, Deferred::Destroyed)
    {
        return;
    }
    match APP.with(|slot| try_with_state(slot, |app| app.apply(message))) {
        AppAccess::Ready(()) => drain_pending(),
        AppAccess::Busy => queue_deferred(message),
        AppAccess::Missing => {}
    }
}

fn drain_pending() {
    if DRAINING.with(|draining| draining.replace(true)) {
        return;
    }
    loop {
        let Some(message) = PENDING.with(|pending| pending.borrow_mut().pop_front()) else {
            break;
        };
        match APP.with(|slot| try_with_state(slot, |app| app.apply(message))) {
            AppAccess::Ready(()) => {}
            AppAccess::Busy => {
                PENDING.with(|pending| pending.borrow_mut().push_front(message));
                break;
            }
            AppAccess::Missing => {
                PENDING.with(|pending| pending.borrow_mut().clear());
                break;
            }
        }
    }
    DRAINING.with(|draining| draining.set(false));
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
    #[serde(default)]
    refresh_minutes: Option<u32>,
    #[serde(default)]
    refresh_enabled: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    approved_volumes: BTreeMap<String, String>,
    #[serde(default)]
    cache_slot: Option<CacheSlot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum CacheSlot {
    A,
    B,
}

impl CacheSlot {
    fn alternate(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    fn path(self, legacy: &Path) -> PathBuf {
        legacy.with_file_name(match self {
            Self::A => "index-a.bin",
            Self::B => "index-b.bin",
        })
    }
}

impl Config {
    fn refresh_preset(&self) -> usize {
        self.refresh_minutes
            .and_then(|minutes| {
                REFRESH_PRESETS
                    .iter()
                    .position(|(value, _)| *value == minutes)
            })
            .unwrap_or(DEFAULT_REFRESH_PRESET)
    }

    fn refresh_delay(&self) -> Option<Duration> {
        let minutes = REFRESH_PRESETS[self.refresh_preset()].0;
        (minutes > 0).then(|| Duration::from_secs(u64::from(minutes) * 60))
    }

    fn can_resume_refresh(&self) -> bool {
        self.refresh_enabled
            && self.refresh_delay().is_some()
            && !self.roots.is_empty()
            && !self.approved_volumes.is_empty()
            && self.cache_slot.is_some()
            && platform::same_root_set(&self.roots, &self.cache_roots)
    }
}

struct ScanSuccess {
    index: Index,
    volume_ids: Option<BTreeMap<String, String>>,
    cache_slot: Option<CacheSlot>,
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

#[derive(Clone, Copy)]
struct ResultSort {
    column: usize,
    descending: bool,
}

impl ResultSort {
    fn compare(self, a: &Entry, b: &Entry) -> cmp::Ordering {
        let order = match self.column {
            0 => a.lower_name().cmp(b.lower_name()),
            1 => compare_entries(a, b),
            2 => a.size.cmp(&b.size),
            _ => a.modified.cmp(&b.modified),
        };
        if self.descending {
            order.reverse()
        } else {
            order
        }
    }
}

struct SearchRequest {
    generation: u64,
    index: Arc<Index>,
    query: String,
    kind: Option<String>,
    sort: Vec<ResultSort>,
}

impl SearchRequest {
    fn run(&self) -> Result<(Vec<Entry>, usize), oilfind_core::Error> {
        let kind = self.kind.as_deref();
        if self.sort.is_empty() {
            return self.index.search(&self.query, kind, RESULT_LIMIT);
        }
        self.index
            .search_by(&self.query, kind, RESULT_LIMIT, |a, b| {
                self.sort
                    .iter()
                    .map(|sort| sort.compare(a, b))
                    .find(|order| !order.is_eq())
                    .unwrap_or(cmp::Ordering::Equal)
            })
    }
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
    search_font: HFONT,
    dpi: i32,
    panel: Panel,
    hotkey: bool,
    search: HWND,
    settings: HWND,
    hide: HWND,
    fixed_roots: HWND,
    refresh_interval: HWND,
    rows_generation: Option<u64>,
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
    /// 扫描进度条（marquee），仅扫描期间可见。
    progress: HWND,
    /// 结果列表上覆盖的空状态提示，仅在没有任何行时可见。
    empty: HWND,
    /// 结果列表的表头控件，用于排序点击与排序箭头。
    header: HWND,
    /// Active header sort first, followed by earlier columns for ties.
    sort: Vec<ResultSort>,
    config: Config,
    config_path: Option<PathBuf>,
    cache_path: Option<PathBuf>,
    storage_guard: Option<Arc<api::StorageGuard>>,
    index: Option<Arc<Index>>,
    rows: Vec<Entry>,
    scan_cancel: Option<Arc<AtomicBool>>,
    scan_gate: Arc<Mutex<()>>,
    scan_generation: u64,
    search_generation: u64,
    scanning: bool,
    auto_sync: bool,
    next_refresh: Option<Instant>,
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
        // 列表视图（结果列表）与进度条（扫描指示）都要注册。
        dwICC: ICC_LISTVIEW_CLASSES | ICC_PROGRESS_CLASS,
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

    let app = match App::new(instance) {
        Ok(app) => app,
        Err(err) => {
            unsafe { CoUninitialize() };
            return Err(err);
        }
    };
    WINDOW_DPI.with(|dpi| dpi.set(app.dpi));
    WINDOW_STATE.with(|state| state.set(WindowState::Starting));
    APP.with(|slot| *slot.borrow_mut() = Some(app));
    let outcome = run_window(instance);
    WINDOW_STATE.with(|state| state.set(WindowState::Stopped));
    let app = APP.with(|slot| slot.borrow_mut().take());
    PENDING.with(|pending| pending.borrow_mut().clear());
    WINDOW_RESULTS.with(|handle| handle.set(std::ptr::null_mut()));
    WINDOW_HEADER.with(|handle| handle.set(std::ptr::null_mut()));
    WINDOW_EMPTY.with(|handle| handle.set(std::ptr::null_mut()));
    WINDOW_GLASS.with(|glass| glass.set(false));
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
        let error = io::Error::last_os_error();
        return Err(format!("注册应用窗口失败：{error}"));
    }

    let title = api::wide("Oil Find for Windows");
    let (dpi, panel) = with_app(|app| (app.dpi, app.panel)).unwrap_or((96, Panel::Search));
    let bounds = panel.bounds(quick_panel::work_area(std::ptr::null_mut(), true), dpi);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT | WS_EX_APPWINDOW,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_BORDER | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        let error = io::Error::last_os_error();
        let init_error = with_app(|app| app.init_error.clone()).flatten();
        return Err(init_error.unwrap_or_else(|| format!("创建应用窗口失败：{error}")));
    }

    WINDOW_GLASS.with(|glass| glass.set(quick_panel::apply_material(hwnd)));
    with_app(|app| app.position_panel(panel, true));
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
    }
    // 窗口显示后再设置焦点：WM_CREATE 里设置的焦点会被随后的框架显示丢弃。
    with_app(|app| {
        let first_focus = if panel == Panel::Settings {
            app.roots_list
        } else {
            app.search
        };
        unsafe { SetFocus(first_focus) };
        app.start_search_worker();
        app.load_cache();
        app.hotkey = unsafe {
            RegisterHotKey(
                hwnd,
                SEARCH_HOTKEY,
                MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
                u32::from(VK_SPACE),
            )
        } != 0;
        app.render_status();
    });

    if WINDOW_STATE.with(Cell::get) == WindowState::Stopped {
        return Err("主窗口在完成启动前关闭。".to_string());
    }
    WINDOW_STATE.with(|state| state.set(WindowState::Running));
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
        if result == -1 {
            let error = io::Error::last_os_error();
            WINDOW_STATE.with(|state| state.set(WindowState::Stopped));
            unsafe { DestroyWindow(hwnd) };
            return Err(format!("读取窗口消息失败：{error}"));
        }
        if result == 0 {
            break;
        }
        let native_key = message.message == WM_KEYDOWN
            && matches!(message.wParam, 0x0D | 0x1B | 0x28)
            && with_app(|app| app.native_key_handling(message.wParam)).unwrap_or(false);
        if !native_key && with_app(|app| app.keyboard(&message)).unwrap_or(false) {
            continue;
        }
        if native_key || unsafe { IsDialogMessageW(hwnd, &message) } == 0 {
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
    match message {
        // WM_CREATE must return its actual success/failure synchronously. This
        // is the only creation of our top-level class, outside an App borrow.
        WM_CREATE => match APP.with(|slot| try_with_state(slot, |app| app.create(hwnd))) {
            AppAccess::Ready(result) => {
                if result == -1 {
                    PENDING.with(|pending| pending.borrow_mut().clear());
                } else {
                    drain_pending();
                }
                result
            }
            AppAccess::Busy => -1,
            AppAccess::Missing => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        },
        // These replies and output pointers must be filled before returning to
        // Win32, even when another App operation is in progress.
        WM_GETMINMAXINFO => {
            if !lparam_is_null(lparam) {
                let info = unsafe { &mut *(lparam as *mut MINMAXINFO) };
                let (width, height) = min_track_size(WINDOW_DPI.with(Cell::get));
                info.ptMinTrackSize.x = width;
                info.ptMinTrackSize.y = height;
            }
            0
        }
        WM_ERASEBKGND if WINDOW_GLASS.with(Cell::get) => {
            let mut area = RECT::default();
            if unsafe { GetClientRect(hwnd, &mut area) } != 0 {
                unsafe { FillRect(wparam as HDC, &area, GetStockObject(BLACK_BRUSH) as _) };
            }
            1
        }
        style::WM_CTLCOLORSTATIC => {
            let empty = WINDOW_EMPTY.with(Cell::get);
            if !empty.is_null() && lparam == empty as isize {
                unsafe { GetSysColorBrush(COLOR_WINDOW) as LRESULT }
            } else if WINDOW_GLASS.with(Cell::get) {
                unsafe {
                    SetBkMode(wparam as HDC, TRANSPARENT as i32);
                    SetTextColor(wparam as HDC, 0x0039_3026);
                    GetStockObject(BLACK_BRUSH) as LRESULT
                }
            } else {
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
        // This is not an owner-data or owner-draw list: item text is stored
        // by the control. Return 0 synchronously, including CDRF_DODEFAULT for
        // NM_CUSTOMDRAW, and never queue drawing/data-request pointers. Copy
        // only our actionable notifications before their lparam expires.
        WM_NOTIFY => {
            let header = WINDOW_HEADER.with(Cell::get);
            let results = WINDOW_RESULTS.with(Cell::get);
            if let Some(notification) = unsafe { read_notification(lparam, header, results) } {
                dispatch_deferred(Deferred::Notify(notification));
            }
            0
        }
        WM_SIZE => {
            dispatch_deferred(Deferred::Size);
            0
        }
        WM_DPICHANGED => {
            if let Some(suggested) = unsafe { read_suggested_rect(lparam) } {
                dispatch_deferred(Deferred::DpiChanged {
                    dpi: (wparam & 0xFFFF) as i32,
                    suggested,
                });
            }
            0
        }
        WM_COMMAND => {
            dispatch_deferred(Deferred::Command {
                id: wparam & 0xFFFF,
                notification: ((wparam >> 16) & 0xFFFF) as u32,
            });
            0
        }
        WM_CONTEXTMENU => {
            dispatch_deferred(Deferred::ContextMenu {
                control: wparam as HWND,
                position: lparam,
            });
            0
        }
        WM_TIMER => {
            dispatch_deferred(Deferred::Timer(wparam));
            0
        }
        WM_HOTKEY if wparam == SEARCH_HOTKEY as usize => {
            dispatch_deferred(Deferred::ToggleSearch);
            0
        }
        WM_APP_EVENT => {
            dispatch_deferred(Deferred::Events);
            0
        }
        WM_APP_PROGRESS => {
            dispatch_deferred(Deferred::Progress);
            0
        }
        WM_CLOSE => {
            dispatch_deferred(Deferred::Close);
            0
        }
        WM_DESTROY => {
            // A failed WM_CREATE also destroys the window. Only request quit
            // for an active message loop so a startup error dialog can display.
            let post_quit = WINDOW_STATE.with(mark_window_stopped);
            PENDING.with(|pending| pending.borrow_mut().clear());
            if post_quit {
                unsafe { PostQuitMessage(0) };
            }
            dispatch_deferred(Deferred::Destroyed);
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn min_track_size(dpi: i32) -> (i32, i32) {
    (280 * dpi / 96, 100 * dpi / 96)
}

unsafe fn read_suggested_rect(lparam: LPARAM) -> Option<RECT> {
    if lparam_is_null(lparam) {
        None
    } else {
        // WM_DPICHANGED owns this pointer only until window_proc returns.
        Some(unsafe { *(lparam as *const RECT) })
    }
}

unsafe fn read_notification(lparam: LPARAM, header: HWND, results: HWND) -> Option<Notification> {
    if lparam_is_null(lparam) {
        return None;
    }
    let notice = unsafe { &*(lparam as *const NMHDR) };
    if !header.is_null() && notice.hwndFrom == header {
        if notice.code as i32 == HDN_ITEMCLICK as i32 {
            let clicked = unsafe { &*(lparam as *const NMHEADERW) };
            return Some(Notification::HeaderClicked(clicked.iItem));
        }
    } else if !results.is_null() && notice.hwndFrom == results {
        if notice.code as i32 == NM_DBLCLK as i32 {
            let clicked = unsafe { &*(lparam as *const NMITEMACTIVATE) };
            return Some(Notification::ResultDoubleClicked(clicked.iItem));
        }
        if notice.code as i32 == LVN_ITEMCHANGED as i32 {
            return Some(Notification::ResultSelectionChanged);
        }
    }
    None
}

impl App {
    fn new(instance: HWND) -> Result<Self, String> {
        let (config_path, cache_path, startup_notice) = platform::config_paths();
        // Acquire before reading either configuration or cache. Memory-only
        // windows have no shared storage and need no lock.
        let storage_guard = config_path
            .as_ref()
            .map(|path| api::StorageGuard::acquire(path).map(Arc::new))
            .transpose()?;
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
            search_font: std::ptr::null_mut(),
            dpi,
            panel: Panel::Search,
            hotkey: false,
            search: std::ptr::null_mut(),
            settings: std::ptr::null_mut(),
            hide: std::ptr::null_mut(),
            fixed_roots: std::ptr::null_mut(),
            refresh_interval: std::ptr::null_mut(),
            rows_generation: None,
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
            progress: std::ptr::null_mut(),
            empty: std::ptr::null_mut(),
            header: std::ptr::null_mut(),
            sort: Vec::new(),
            config: Config::default(),
            config_path,
            cache_path,
            storage_guard,
            index: None,
            rows: Vec::new(),
            scan_cancel: None,
            scan_gate: Arc::new(Mutex::new(())),
            scan_generation: 0,
            search_generation: 0,
            scanning: false,
            auto_sync: false,
            next_refresh: None,
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
        if app.config.cache_roots.is_empty() {
            app.panel = Panel::Settings;
        }
        Ok(app)
    }

    fn create(&mut self, hwnd: HWND) -> LRESULT {
        self.hwnd = hwnd;
        match self.create_ui(hwnd) {
            Ok(()) => 0,
            Err(err) => {
                self.init_error = Some(err);
                -1
            }
        }
    }

    fn apply(&mut self, message: Deferred) {
        if self.closing && !matches!(message, Deferred::Destroyed) {
            return;
        }
        match message {
            Deferred::Size => {
                if !self.results.is_null() && unsafe { IsIconic(self.hwnd) } == 0 {
                    let desired =
                        Panel::for_query(self.panel == Panel::Settings, &api::text(self.search));
                    if self.panel == desired {
                        self.layout();
                    } else {
                        self.position_panel(desired, false);
                    }
                }
            }
            Deferred::DpiChanged { dpi, suggested } => {
                if !self.results.is_null() {
                    if let Err(err) = self.change_dpi(dpi, suggested) {
                        self.last_notice = format!("调整显示缩放失败，保留现有界面：{err}");
                        self.render_status();
                    }
                }
            }
            Deferred::Command { id, notification } => self.command(id, notification),
            Deferred::ContextMenu { control, position } => self.context_menu(control, position),
            Deferred::Notify(notification) => self.notify(notification),
            Deferred::Timer(timer) => {
                if timer == TIMER_SEARCH {
                    unsafe { KillTimer(self.hwnd, TIMER_SEARCH) };
                    self.search_now();
                } else if timer == TIMER_SYNC
                    && self.auto_sync
                    && self.next_refresh.is_some_and(|due| Instant::now() >= due)
                {
                    self.disarm_refresh();
                    self.begin_scan(true);
                }
            }
            Deferred::Events => self.drain_events(),
            Deferred::Progress => self.show_progress(),
            Deferred::ToggleSearch => self.toggle_search_panel(),
            Deferred::Close => {
                self.shutdown();
                unsafe { DestroyWindow(self.hwnd) };
            }
            Deferred::Destroyed => self.shutdown(),
        }
    }

    fn create_ui(&mut self, hwnd: HWND) -> Result<(), String> {
        let window_dpi = unsafe { GetDpiForWindow(hwnd) };
        if (96..=480).contains(&window_dpi) {
            self.dpi = window_dpi as i32;
        }
        WINDOW_DPI.with(|dpi| dpi.set(self.dpi));
        self.font = api::create_font(self.dpi, 17)?;
        self.search_font = api::create_font(self.dpi, 24)?;

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
        for label in ["Oil Find", "索引范围", "文件类型", "刷新间隔"] {
            let handle = make("STATIC", label, style::SS_LEFT, 0, 0)?;
            self.labels.push(handle);
        }
        self.search = make(
            "EDIT",
            "",
            style::WS_TABSTOP | style::WS_BORDER | style::ES_AUTOHSCROLL,
            0,
            ID_SEARCH,
        )?;
        api::set_font(self.search, self.search_font);
        self.set_search_margins();
        api::send_message(self.search, style::EM_SETLIMITTEXT, 4096, 0);
        let cue = api::wide("搜索文件名、路径或拼音");
        api::send_message(
            self.search,
            style::EM_SETCUEBANNER,
            0,
            cue.as_ptr() as isize,
        );
        self.settings = make("BUTTON", "索引设置", style::WS_TABSTOP, 0, ID_SETTINGS)?;
        self.hide = make("BUTTON", "最小化", style::WS_TABSTOP, 0, ID_HIDE)?;
        self.fixed_roots = make("BUTTON", "本机固定盘", style::WS_TABSTOP, 0, ID_FIXED_ROOTS)?;
        self.refresh_interval = make(
            "COMBOBOX",
            "",
            style::WS_TABSTOP | style::WS_VSCROLL | style::CBS_DROPDOWNLIST,
            0,
            ID_REFRESH_INTERVAL,
        )?;
        for (_, label) in REFRESH_PRESETS {
            let label = api::wide(label);
            api::send_message(
                self.refresh_interval,
                style::CB_ADDSTRING,
                0,
                label.as_ptr() as isize,
            );
        }
        api::send_message(
            self.refresh_interval,
            style::CB_SETCURSEL,
            self.config.refresh_preset(),
            0,
        );
        for (label, id) in [
            ("添加目录…", ID_ADD_ROOT),
            ("删除范围", ID_REMOVE_ROOT),
            ("开始索引", ID_START),
            ("暂停刷新", ID_STOP),
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
            style::RESULTS_STYLE,
            style::WS_EX_CLIENTEDGE,
            ID_RESULTS,
        )?;
        WINDOW_RESULTS.with(|handle| handle.set(self.results));
        api::send_message(
            self.results,
            style::LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (style::LVS_EX_FULLROWSELECT | style::LVS_EX_DOUBLEBUFFER) as isize,
        );
        // 表头句柄：排序点击通知由列表视图转发给父窗口。
        self.header = api::send_message(self.results, style::LVM_GETHEADER, 0, 0) as HWND;
        WINDOW_HEADER.with(|handle| handle.set(self.header));
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

        // 扫描进度条：indeterminate marquee，平时隐藏，扫描时显示。
        self.progress = make(progress_control_class(), "", style::PBS_MARQUEE, 0, 0)?;
        unsafe { ShowWindow(self.progress, SW_HIDE) };

        // 空状态提示：覆盖在结果列表上方，仅在没有行时显示（后创建 = 上层）。
        self.empty = make("STATIC", "", style::SS_CENTER, 0, 0)?;
        WINDOW_EMPTY.with(|handle| handle.set(self.empty));
        unsafe { ShowWindow(self.empty, SW_HIDE) };

        self.refresh_roots();
        self.selection_buttons();
        api::enable(self.stop, false);
        self.last_notice = if self.config.roots.is_empty() {
            "在索引设置里选择范围，再开始索引。".to_string()
        } else {
            "确认索引范围后点击开始。索引成功后按所选间隔刷新。".to_string()
        };
        if !self.startup_notice.is_empty() {
            self.storage_notice = self.startup_notice.clone();
        }
        self.render_status();
        self.refresh_header();
        self.refresh_empty_state();
        self.layout();
        Ok(())
    }

    fn px(&self, value: i32) -> i32 {
        value * self.dpi / 96
    }

    fn set_search_margins(&self) {
        let margin = self.px(12) as isize;
        api::send_message(
            self.search,
            style::EM_SETMARGINS,
            3,
            margin | (margin << 16),
        );
    }

    fn layout(&mut self) {
        let mut area = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut area) } == 0 {
            return;
        }
        let settings = self.panel == Panel::Settings;
        let results = self.panel == Panel::Results;
        let show = |controls: &[HWND], visible: bool| {
            let command = if visible {
                windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNA
            } else {
                SW_HIDE
            };
            for &handle in controls {
                unsafe { ShowWindow(handle, command) };
            }
        };
        show(&[self.search], !settings);
        show(
            &[
                self.results,
                self.types,
                self.labels[2],
                self.open,
                self.reveal,
                self.copy,
            ],
            results,
        );
        show(
            &[
                self.labels[1],
                self.roots_list,
                self.add,
                self.remove,
                self.fixed_roots,
                self.labels[3],
                self.refresh_interval,
                self.stop,
            ],
            settings,
        );
        show(&[self.start], settings);
        self.refresh_empty_state();

        let p = self.px(22);
        let w = area.right;
        let h = area.bottom;
        let right = w - p;
        let content_width = w - 2 * p;
        let row = self.px(32);
        let gap = self.px(10);
        let place = |handle: HWND, x, y, width: i32, height: i32| {
            unsafe { MoveWindow(handle, x, y, width.max(1), height.max(1), 1) };
        };

        let top = self.px(13);
        let hide_width = self.px(76);
        let settings_width = self.px(102);
        let settings_x = right - hide_width - gap - settings_width;
        place(self.labels[0], p, top + self.px(4), settings_x - p, row);
        place(self.settings, settings_x, top, settings_width, row);
        place(self.hide, right - hide_width, top, hide_width, row);
        place(self.status, p, h - self.px(29), content_width, self.px(24));
        place(self.progress, 0, h - self.px(4), w, self.px(4));

        let body = self.px(57);
        let start_width = self.px(112);
        if settings {
            let button_width = self.px(132);
            let list_y = body + self.px(30);
            let settings_footer = h - self.px(81);
            let list_height = (settings_footer - list_y - gap * 2).max(self.px(112));
            place(self.labels[1], p, body, content_width, row);
            place(
                self.roots_list,
                p,
                list_y,
                content_width - button_width - gap,
                list_height,
            );
            for (i, handle) in [self.fixed_roots, self.add, self.remove]
                .into_iter()
                .enumerate()
            {
                place(
                    handle,
                    right - button_width,
                    list_y + i as i32 * (row + gap),
                    button_width,
                    row,
                );
            }
            place(
                self.labels[3],
                p,
                settings_footer + self.px(5),
                self.px(76),
                row,
            );
            place(
                self.refresh_interval,
                p + self.px(82),
                settings_footer,
                self.px(142),
                self.px(230),
            );
            place(
                self.stop,
                right - start_width * 2 - gap,
                settings_footer,
                start_width,
                row,
            );
            place(
                self.start,
                right - start_width,
                settings_footer,
                start_width,
                row,
            );
            return;
        }

        let edit_height = self.px(43);
        place(self.search, p, body, content_width, edit_height);
        if !results {
            return;
        }
        let types_y = body + edit_height + gap;
        let types_width = self.px(126);
        place(
            self.labels[2],
            right - types_width - self.px(76),
            types_y + self.px(5),
            self.px(72),
            row,
        );
        place(
            self.types,
            right - types_width,
            types_y,
            types_width,
            self.px(226),
        );

        let list_y = types_y + row + gap;
        let buttons_y = h - self.px(70);
        let list_height = buttons_y - list_y - gap;
        place(self.results, p, list_y, content_width, list_height);
        place(
            self.empty,
            p + 1,
            list_y + list_height / 2,
            content_width - 2,
            row,
        );
        place(self.open, p, buttons_y, self.px(104), row);
        place(self.reveal, p + self.px(115), buttons_y, self.px(156), row);
        place(self.copy, p + self.px(282), buttons_y, self.px(104), row);
        for (position, column_width) in [
            self.px(199),
            (content_width - self.px(458)).max(self.px(80)),
            self.px(84),
            self.px(175),
        ]
        .into_iter()
        .enumerate()
        {
            api::send_message(
                self.results,
                style::LVM_SETCOLUMNWIDTH,
                position,
                column_width as isize,
            );
        }
    }

    fn position_panel(&mut self, panel: Panel, follow_pointer: bool) {
        self.panel = panel;
        let (settings_label, hide_label) = if panel == Panel::Settings {
            ("返回搜索", "退出")
        } else {
            ("索引设置", "最小化")
        };
        api::put_text(self.settings, settings_label);
        api::put_text(self.hide, hide_label);
        let bounds = panel.bounds(quick_panel::work_area(self.hwnd, follow_pointer), self.dpi);
        unsafe {
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.layout();
    }

    fn toggle_settings(&mut self) {
        if self.panel == Panel::Settings {
            let panel = Panel::for_query(false, &api::text(self.search));
            self.position_panel(panel, false);
            unsafe { SetFocus(self.search) };
        } else {
            self.position_panel(Panel::Settings, false);
            unsafe { SetFocus(self.roots_list) };
        }
        self.render_status();
    }

    fn toggle_search_panel(&mut self) {
        if self.panel != Panel::Settings
            && unsafe { IsIconic(self.hwnd) == 0 && GetForegroundWindow() == self.hwnd }
        {
            unsafe { ShowWindow(self.hwnd, SW_MINIMIZE) };
            return;
        }
        if unsafe { IsIconic(self.hwnd) } != 0 {
            unsafe { ShowWindow(self.hwnd, SW_RESTORE) };
        }
        let panel = Panel::for_query(false, &api::text(self.search));
        self.position_panel(panel, true);
        unsafe {
            SetForegroundWindow(self.hwnd);
            SetFocus(self.search);
        }
        api::send_message(self.search, style::EM_SETSEL, 0, -1);
        self.render_status();
    }

    fn change_dpi(&mut self, dpi: i32, suggested: RECT) -> Result<(), String> {
        if !(96..=768).contains(&dpi)
            || suggested.right <= suggested.left
            || suggested.bottom <= suggested.top
        {
            return Err("显示缩放参数无效".to_string());
        }
        let font = api::create_font(dpi, 17)?;
        let search_font = match api::create_font(dpi, 24) {
            Ok(font) => font,
            Err(error) => {
                api::delete_font(font);
                return Err(error);
            }
        };
        let previous = self.font;
        let previous_search = self.search_font;
        self.font = font;
        self.search_font = search_font;
        self.dpi = dpi;
        WINDOW_DPI.with(|snapshot| snapshot.set(dpi));
        let mut controls = self.labels.clone();
        controls.extend([
            self.settings,
            self.hide,
            self.fixed_roots,
            self.refresh_interval,
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
            self.progress,
            self.empty,
        ]);
        for handle in controls {
            api::set_font(handle, font);
        }
        api::set_font(self.search, search_font);
        self.set_search_margins();
        // 表头不继承列表视图字体，显式设置。
        if !self.header.is_null() {
            api::set_font(self.header, font);
        }
        // Fit the panel on the destination monitor at its new DPI.
        let bounds = self
            .panel
            .bounds(quick_panel::work_area_near(&suggested), dpi);
        unsafe {
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.layout();
        api::delete_font(previous);
        api::delete_font(previous_search);
        Ok(())
    }

    fn keyboard(&mut self, message: &MSG) -> bool {
        if message.message != WM_KEYDOWN {
            return false;
        }
        let focus = unsafe { GetFocus() };
        if message.wParam == 0x1B {
            if self.panel == Panel::Settings {
                self.toggle_settings();
            } else {
                unsafe { ShowWindow(self.hwnd, SW_MINIMIZE) };
            }
            return true;
        }
        let control = unsafe { GetKeyState(0x11) };
        if self.panel != Panel::Settings && message.wParam == 'L' as usize && control < 0 {
            unsafe { SetFocus(self.search) };
            api::send_message(self.search, style::EM_SETSEL, 0, -1);
            return true;
        }
        if message.wParam == 'C' as usize && control < 0 {
            // Ctrl+C：焦点在结果列表时复制所选路径。
            if focus == self.results {
                self.file_action(ID_COPY);
                return true;
            }
        }
        if message.wParam == 0x28
            && focus == self.search
            && self.rows_generation == Some(self.search_generation)
            && !self.rows.is_empty()
        {
            self.select_first_result();
            unsafe { SetFocus(self.results) };
            return true;
        }
        if message.wParam == 0x0D {
            if focus == self.results {
                self.file_action(ID_OPEN);
                return true;
            }
            if focus == self.search {
                if self.rows_generation == Some(self.search_generation) && !self.rows.is_empty() {
                    self.select_first_result();
                    self.file_action(ID_OPEN);
                    return true;
                }
                unsafe { KillTimer(self.hwnd, TIMER_SEARCH) };
                self.search_now();
                return true;
            }
        }
        false
    }

    fn native_key_handling(&self, key: usize) -> bool {
        let focus = unsafe { GetFocus() };
        (focus == self.search && quick_panel::composing(self.search))
            || (key == 0x1B
                && (focus == self.types || focus == self.refresh_interval)
                && api::send_message(focus, style::CB_GETDROPPEDSTATE, 0, 0) != 0)
    }

    fn select_first_result(&self) {
        if self.selected_entry().is_some() || self.rows.is_empty() {
            return;
        }
        let item = LVITEMW {
            state: style::LVIS_SELECTED,
            stateMask: style::LVIS_SELECTED,
            ..Default::default()
        };
        api::send_message(
            self.results,
            style::LVM_SETITEMSTATE,
            0,
            &item as *const LVITEMW as isize,
        );
    }

    fn command(&mut self, id: usize, notification: u32) {
        if self.closing {
            return;
        }
        match id {
            ID_SETTINGS if notification == 0 => self.toggle_settings(),
            ID_HIDE if notification == 0 && self.panel == Panel::Settings => {
                self.apply(Deferred::Close)
            }
            ID_HIDE if notification == 0 => {
                unsafe { ShowWindow(self.hwnd, SW_MINIMIZE) };
            }
            ID_FIXED_ROOTS if notification == 0 => self.use_fixed_roots(),
            ID_REFRESH_INTERVAL if notification == 1 => self.change_refresh_setting(),
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
            ID_START if notification == 0 && self.panel == Panel::Settings => {
                if self.config.roots.is_empty() {
                    api::warn(
                        self.hwnd,
                        "请先添加至少一个本机目录；应用不会自动选择或扫描个人目录。",
                    );
                    return;
                }
                self.disarm_refresh();
                self.auto_sync = self.config.refresh_delay().is_some();
                self.begin_scan(false);
            }
            ID_STOP if notification == 0 => {
                self.pause_scan();
                self.last_notice = "已暂停刷新，仍可搜索已有索引。".to_string();
                self.persist_refresh_setting();
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

    fn notify(&mut self, notification: Notification) {
        match notification {
            Notification::HeaderClicked(column) => self.sort_by_column(column as usize),
            Notification::ResultDoubleClicked(item) => {
                if item >= 0 && (item as usize) < self.rows.len() {
                    self.file_action(ID_OPEN);
                }
            }
            Notification::ResultSelectionChanged => self.selection_buttons(),
        }
    }

    /// 点击表头：新列默认升序，重复点击同一列反转方向。
    fn sort_by_column(&mut self, column: usize) {
        if column >= 4 {
            return;
        }
        let descending = self
            .sort
            .first()
            .is_some_and(|sort| sort.column == column && !sort.descending);
        // A stable header sort keeps earlier column orders for equal values.
        self.sort.retain(|sort| sort.column != column);
        self.sort.insert(0, ResultSort { column, descending });
        self.refresh_header();
        if self.index.is_some() {
            self.queue_search();
        }
    }

    /// 重画表头文字：排序列带 ▲/▼，其余列显示纯名称。
    fn refresh_header(&self) {
        if self.header.is_null() {
            return;
        }
        const LABELS: [&str; 4] = ["文件名", "完整路径", "大小", "修改日期"];
        for (position, label) in LABELS.iter().enumerate() {
            let mut text = (*label).to_string();
            if let Some(sort) = self.sort.first().filter(|sort| sort.column == position) {
                text.push(if sort.descending { '▼' } else { '▲' });
            }
            let buffer = api::wide(&text);
            let item = HDITEMW {
                mask: HDI_TEXT,
                cxy: 0,
                pszText: buffer.as_ptr() as *mut u16,
                hbm: std::ptr::null_mut(),
                cchTextMax: buffer.len() as i32,
                fmt: 0,
                lParam: 0,
                iImage: 0,
                iOrder: 0,
                r#type: 0,
                pvFilter: std::ptr::null_mut(),
                state: 0,
            };
            api::send_message(
                self.header,
                style::HDM_SETITEMW,
                position,
                &item as *const HDITEMW as isize,
            );
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
        self.config.approved_volumes.clear();
        self.last_update = None;
        self.set_rows(Vec::new());
    }

    fn use_fixed_roots(&mut self) {
        let roots = platform::local_fixed_roots();
        if roots.is_empty() {
            self.last_notice = "没有找到可用的本机固定盘。".to_string();
            self.render_status();
        } else if !platform::same_root_set(&self.config.roots, &roots) {
            self.invalidate_scope();
            self.config.roots = roots;
            self.scope_changed("已选中本机固定盘。确认范围后点击开始索引。");
        }
    }

    fn change_refresh_setting(&mut self) {
        let selected = api::send_message(self.refresh_interval, style::CB_GETCURSEL, 0, 0);
        let Some(&(minutes, label)) = REFRESH_PRESETS.get(selected as usize) else {
            return;
        };
        self.config.refresh_minutes = Some(minutes);
        if minutes == 0 {
            self.auto_sync = false;
            self.config.refresh_enabled = false;
        }
        self.last_notice = if self.auto_sync || minutes == 0 {
            format!("刷新间隔已设为{label}。")
        } else {
            format!("刷新间隔已设为{label}。点击开始索引后生效。")
        };
        self.schedule_next_refresh();
        api::enable(self.stop, self.auto_sync || self.scanning);
        self.persist_refresh_setting();
        self.render_status();
    }

    fn scope_changed(&mut self, notice: &str) {
        self.last_notice = notice.to_string();
        if let Err(err) = self.save_configuration() {
            self.storage_notice = format!("范围未保存。重启会读取上次保存的范围和刷新设置：{err}");
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

    fn begin_scan(&mut self, automatic: bool) {
        if self.scanning || self.config.roots.is_empty() {
            return;
        }
        self.scan_generation += 1;
        let generation = self.scan_generation;
        let roots: Vec<PathBuf> = self.config.roots.iter().map(PathBuf::from).collect();
        let approved = automatic.then(|| self.config.approved_volumes.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        self.scan_cancel = Some(Arc::clone(&cancel));
        self.scanning = true;
        api::enable(self.start, false);
        api::enable(self.stop, true);
        self.last_notice = "正在建立本机元数据快照；界面仍可操作。".to_string();
        self.render_status();
        self.set_scan_progress(true);

        let sender = self.events_tx.clone();
        let hwnd = SendHwnd(self.hwnd);
        // Write to the unreferenced slot. The atomic config write publishes the
        // new snapshot and its volume approval together.
        let cache_slot = self.cache_path.as_ref().map(|_| {
            self.config
                .cache_slot
                .map_or(CacheSlot::A, CacheSlot::alternate)
        });
        let cache_path = self
            .cache_path
            .as_ref()
            .zip(cache_slot)
            .map(|(path, slot)| slot.path(path));
        let gate = Arc::clone(&self.scan_gate);
        let progress_slot = Arc::clone(&self.progress_slot);
        let storage_guard = self.storage_guard.clone();
        std::thread::spawn(move || {
            // Closing the window cancels but does not join this worker. Keep
            // the process-wide storage lock until it can no longer write.
            let _storage_guard = storage_guard;
            // 同一时刻只允许一个扫描线程写缓存；被取消的旧任务会很快释放。
            let _guard = lock(&gate);
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            let preflight = (|| {
                for root in &roots {
                    platform::local_directory(root)?;
                }
                let volumes = platform::volume_ids(&roots).ok();
                if approved
                    .as_ref()
                    .is_some_and(|ids| volumes.as_ref() != Some(ids))
                {
                    return Err("索引范围所在的磁盘已变化，请重新确认后开始索引。".to_string());
                }
                Ok(volumes)
            })();
            let volume_ids = match preflight {
                Ok(ids) => ids,
                Err(err) => {
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
            };
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
            if volume_ids
                .as_ref()
                .is_some_and(|ids| platform::volume_ids(&roots).ok().as_ref() != Some(ids))
            {
                send_event(
                    &sender,
                    hwnd,
                    Event::Scan {
                        generation,
                        outcome: Err("扫描期间磁盘发生变化，已保留旧索引。".to_string()),
                    },
                );
                return;
            }
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
                        volume_ids,
                        cache_slot,
                        save_error,
                        updated: SystemTime::now(),
                    }),
                },
            );
        });
    }

    fn pause_scan(&mut self) {
        self.auto_sync = false;
        self.config.refresh_enabled = false;
        self.disarm_refresh();
        self.scan_generation += 1;
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        self.scanning = false;
        self.set_scan_progress(false);
        api::enable(self.start, !self.config.roots.is_empty());
        api::enable(self.stop, false);
    }

    fn disarm_refresh(&mut self) {
        unsafe { KillTimer(self.hwnd, TIMER_SYNC) };
        self.next_refresh = None;
    }

    fn schedule_next_refresh(&mut self) {
        self.disarm_refresh();
        let Some(delay) = self
            .config
            .refresh_delay()
            .filter(|_| self.auto_sync && !self.scanning)
        else {
            return;
        };
        let deadline = Instant::now() + delay;
        if unsafe { SetTimer(self.hwnd, TIMER_SYNC, delay.as_millis() as u32, None) } == 0 {
            self.auto_sync = false;
            self.config.refresh_enabled = false;
            api::enable(self.stop, false);
            self.last_notice = "无法设置定时刷新，请手动开始索引。".to_string();
            self.persist_refresh_setting();
            return;
        }
        self.next_refresh = Some(deadline);
    }

    fn persist_refresh_setting(&mut self) {
        if self.config_path.is_some() {
            if let Err(err) = self.save_configuration() {
                self.storage_notice = format!("刷新设置未保存。重启会读取上次保存的设置：{err}");
            }
        }
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
            let outcome = request.run().map_err(|err| err.to_string());
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
        // A fresh result set uses relevance for ties in the active column.
        self.sort.truncate(1);
        self.queue_search();
    }

    fn queue_search(&mut self) {
        self.search_generation += 1;
        self.selection_buttons();
        let query = api::text(self.search).trim().to_string();
        let panel = Panel::for_query(self.panel == Panel::Settings, &query);
        if panel != self.panel && unsafe { IsIconic(self.hwnd) } == 0 {
            self.position_panel(panel, false);
        }
        if query.is_empty() {
            self.rows_generation = None;
            self.set_rows(Vec::new());
            self.layout();
            self.render_status();
            return;
        }
        let Some(index) = self.index.clone() else {
            self.set_rows(Vec::new());
            self.last_notice = "在索引设置里确认范围，再开始索引。".to_string();
            self.render_status();
            return;
        };
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
            sort: self.sort.clone(),
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
                // 无论成功、失败还是保存未提交，扫描都已结束。
                self.set_scan_progress(false);
                self.scanning = false;
                self.scan_cancel = None;
                api::enable(self.start, !self.config.roots.is_empty());
                api::enable(self.stop, self.auto_sync);
                let success = match outcome {
                    Ok(success) => success,
                    Err(err) => {
                        // 失败就停掉 30 秒定时器：反复重试失败任务只会白烧 CPU。
                        self.auto_sync = false;
                        self.config.refresh_enabled = false;
                        self.disarm_refresh();
                        api::enable(self.stop, false);
                        self.last_notice = format!(
                            "索引失败，已有缓存仍可使用；自动同步已暂停，请处理后再手动开始：{err}"
                        );
                        self.persist_refresh_setting();
                        self.render_status();
                        return;
                    }
                };
                // 缓存未持久化时不能替换当前索引：旧索引与旧搜索结果继续可用。
                if self.cache_path.is_some() {
                    if let Some(err) = success.save_error {
                        self.last_notice = "本次索引未提交，保留原索引与搜索结果。".to_string();
                        self.storage_notice = format!("缓存未保存：{err}");
                        if self.config.approved_volumes.is_empty() {
                            self.auto_sync = false;
                            api::enable(self.stop, false);
                        }
                        self.schedule_next_refresh();
                        self.render_status();
                        return;
                    }
                }
                let count = success.index.len();
                self.index = Some(Arc::new(success.index));
                self.last_update = Some(success.updated);
                self.storage_notice.clear();
                self.last_notice = format!("索引完成：{count} 项。");
                self.config.approved_volumes = success.volume_ids.unwrap_or_default();
                if self.auto_sync && self.config.approved_volumes.is_empty() {
                    self.auto_sync = false;
                    api::enable(self.stop, false);
                    self.last_notice
                        .push_str("卷标识无法验证，定时刷新已暂停。");
                }
                if self.cache_path.is_none() {
                    // 启动时已明确降级为仅内存模式：快照可用，但没有有效缓存范围。
                    self.storage_notice =
                        "仅内存索引，退出后丢失；配置与缓存未持久化。".to_string();
                    self.config.cache_roots.clear();
                    self.config.refresh_enabled = false;
                } else {
                    // 只有真正持久化成功，cache_roots 才代表下次启动可加载的范围。
                    self.config.cache_roots = self.config.roots.clone();
                    self.config.refresh_enabled = self.auto_sync;
                    let previous_slot = self.config.cache_slot;
                    self.config.cache_slot = success.cache_slot;
                    if let Err(err) = self.save_configuration() {
                        self.config.cache_slot = previous_slot;
                        self.config.cache_roots.clear();
                        self.config.refresh_enabled = false;
                        self.storage_notice = format!("配置未保存：{err}");
                    }
                }
                self.schedule_next_refresh();
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
                self.set_scan_progress(false);
                match outcome {
                    Ok((index, updated)) => {
                        let count = index.len();
                        self.index = Some(Arc::new(index));
                        self.last_update = Some(updated);
                        self.auto_sync = self.config.can_resume_refresh();
                        self.last_notice = format!("已加载本机索引（{count} 项）。");
                        self.schedule_next_refresh();
                        api::enable(self.stop, self.auto_sync);
                        self.render_status();
                        self.search_now();
                    }
                    Err(err) => {
                        self.last_notice = format!("未加载索引缓存：{err}。请手动开始索引。");
                        self.refresh_empty_state();
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
                        self.rows_generation = Some(generation);
                        self.set_rows(entries);
                        self.last_notice =
                            format!("共命中 {total} 项，显示 {} 项", self.rows.len());
                        if total > RESULT_LIMIT {
                            self.last_notice
                                .push_str(&format!("（最多 {RESULT_LIMIT} 项，请缩小搜索条件）"));
                        }
                    }
                    Err(err) => {
                        self.rows_generation = None;
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
            let name = api::wide(entry.name());
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
                entry.path().to_string_lossy().to_string(),
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
        self.refresh_empty_state();
    }

    /// 扫描期间显示底部 indeterminate 进度条；其余时间隐藏。
    fn set_scan_progress(&self, active: bool) {
        if self.progress.is_null() {
            return;
        }
        unsafe {
            ShowWindow(self.progress, if active { SW_SHOW } else { SW_HIDE });
        }
        // PBM_SETMARQUEE：wParam 开关，lParam 为动画帧间隔（毫秒）。
        api::send_message(self.progress, style::PBM_SETMARQUEE, active as usize, 30);
    }

    /// 没有任何结果行时，在结果区显示引导文案；有行则隐藏。
    fn refresh_empty_state(&self) {
        if self.empty.is_null() {
            return;
        }
        if self.panel != Panel::Results {
            unsafe { ShowWindow(self.empty, SW_HIDE) };
            return;
        }
        let text = if self.index.is_none() {
            "还没有可用索引。请添加本机目录，然后点击“开始索引”。"
        } else if self.rows.is_empty() {
            if api::text(self.search).trim().is_empty() {
                "索引中没有条目。请检查索引范围，然后重新索引。"
            } else {
                "没有匹配的结果。试试拼音首字母（如 hyjy）、ext:pdf 或 size:>1mb。"
            }
        } else {
            ""
        };
        if text.is_empty() {
            unsafe { ShowWindow(self.empty, SW_HIDE) };
        } else {
            api::put_text(self.empty, text);
            unsafe { ShowWindow(self.empty, SW_SHOW) };
        }
    }

    /// 窗口标题反映索引规模，便于在多窗口间辨认。
    fn refresh_title(&self) {
        if self.hwnd.is_null() {
            return;
        }
        let count = self.index.as_ref().map_or(0, |index| index.len());
        let title = if count == 0 {
            "Oil Find for Windows".to_string()
        } else {
            format!("Oil Find for Windows · 索引 {} 项", group_digits(count))
        };
        let buffer = api::wide(&title);
        unsafe { SetWindowTextW(self.hwnd, buffer.as_ptr()) };
    }

    /// 结果列表右键菜单：打开 / 资源管理器定位 / 复制路径。
    ///
    /// 右键落到哪一项就先选中哪一项（与资源管理器行为一致）；
    /// 键盘唤出（Shift+F10）时菜单出现在列表可见区域中央。
    fn context_menu(&mut self, control: HWND, lparam: LPARAM) {
        if control != self.results || self.results.is_null() {
            return;
        }
        let (mut point, mouse) = match context_menu_position(lparam) {
            Some((x, y)) => (POINT { x, y }, true),
            None => (POINT { x: 0, y: 0 }, false),
        };
        if mouse {
            let mut client = point;
            if unsafe { ScreenToClient(self.results, &mut client) } == 0 {
                return;
            }
            // LVM_HITTEST writes flags and item details back to this structure.
            let mut hit = LVHITTESTINFO {
                pt: client,
                flags: 0,
                iItem: -1,
                iSubItem: -1,
                iGroup: -1,
            };
            let item = api::send_message(
                self.results,
                style::LVM_HITTEST,
                0,
                &mut hit as *mut LVHITTESTINFO as isize,
            );
            if item < 0 {
                return;
            }
            let selection = LVITEMW {
                state: style::LVIS_SELECTED,
                stateMask: style::LVIS_SELECTED,
                iItem: item as i32,
                ..Default::default()
            };
            api::send_message(
                self.results,
                style::LVM_SETITEMSTATE,
                item as usize,
                &selection as *const LVITEMW as isize,
            );
            self.selection_buttons();
        } else {
            let mut rect = RECT::default();
            if unsafe { GetClientRect(self.results, &mut rect) } == 0 {
                return;
            }
            point.x = (rect.left + rect.right) / 2;
            point.y = (rect.top + rect.bottom) / 2;
            let mut screen = point;
            if unsafe { ClientToScreen(self.results, &mut screen) } == 0 {
                return;
            }
            point = screen;
        }
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            return;
        }
        // 标签缓冲区必须活到 AppendMenuW 返回，故先统一准备好。
        let items: Vec<(usize, Vec<u16>)> = [
            (ID_OPEN, "打开"),
            (ID_REVEAL, "资源管理器定位"),
            (ID_COPY, "复制路径"),
        ]
        .into_iter()
        .map(|(id, label)| (id, api::wide(label)))
        .collect();
        for (id, label) in &items {
            unsafe { AppendMenuW(menu, MF_STRING, *id, label.as_ptr()) };
        }
        // TPM_RETURNCMD：同步返回所选条目 ID，取消时为 0。
        let chosen = unsafe {
            TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_LEFTALIGN,
                point.x,
                point.y,
                0,
                self.hwnd,
                std::ptr::null_mut(),
            )
        };
        unsafe { DestroyMenu(menu) };
        if chosen > 0 {
            self.file_action(chosen as usize);
        }
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
        let selected =
            self.rows_generation == Some(self.search_generation) && self.selected_entry().is_some();
        api::enable(self.open, selected);
        api::enable(self.reveal, selected);
        api::enable(self.copy, selected);
    }

    fn file_action(&mut self, action: usize) {
        if self.rows_generation != Some(self.search_generation) {
            return;
        }
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let path = entry.path();
        if let Err(err) = platform::local_path(&path) {
            api::warn(self.hwnd, &err);
            return;
        }
        if !self.in_scope(&path) {
            api::warn(self.hwnd, "该项目已不在当前索引范围内，请重新索引。");
            return;
        }
        if !self.config.approved_volumes.is_empty()
            && !platform::volume_ids(std::slice::from_ref(&path)).is_ok_and(|observed| {
                observed
                    .iter()
                    .all(|(drive, id)| self.config.approved_volumes.get(drive) == Some(id))
            })
        {
            api::warn(self.hwnd, "此结果所在的磁盘已变化。请核对范围后重新索引。");
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
        let mut mode = "手动刷新".to_string();
        if self.auto_sync {
            let label = REFRESH_PRESETS[self.config.refresh_preset()].1;
            mode = format!("{label}刷新");
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
        let shortcut = if self.hotkey {
            "Ctrl+Alt+Space"
        } else {
            "全局快捷键不可用"
        };
        let text = if self.panel == Panel::Search
            && self.index.is_some()
            && !self.scanning
            && self.storage_notice.is_empty()
        {
            format!("已索引 {} 项 · {mode} · {shortcut}", group_digits(count))
        } else if self.panel == Panel::Search {
            format!("{} · {shortcut}", notice.trim())
        } else {
            format!(
                "{} · 索引 {} 项{updated} · {notice}",
                mode,
                group_digits(count)
            )
        };
        let buffer = api::wide(&text);
        unsafe {
            SetWindowTextW(self.status, buffer.as_ptr());
        }
        self.refresh_title();
    }

    fn load_configuration(&mut self) {
        let Some(config_path) = self.config_path.clone() else {
            self.config.roots = platform::local_fixed_roots();
            return;
        };
        if platform::is_link_or_reparse(&config_path) {
            self.startup_notice = "目录配置是链接，未加载；请重新添加本机目录。".to_string();
            return;
        }
        let data = match std::fs::read(&config_path) {
            Ok(data) => data,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.config.roots = platform::local_fixed_roots();
                return;
            }
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
            self.config.refresh_enabled = config.refresh_enabled;
            self.config.approved_volumes = config.approved_volumes;
        }
        self.config.refresh_minutes = config.refresh_minutes;
        self.config.cache_slot = config.cache_slot;
        self.config.roots = roots;
    }

    fn save_configuration(&self) -> Result<(), String> {
        let Some(config_path) = self.config_path.as_ref() else {
            return Err("当前用户配置目录不可用".to_string());
        };
        let data = serde_json::to_vec_pretty(&self.config).map_err(|err| err.to_string())?;
        api::write_configuration(config_path, &data).map_err(|err| err.to_string())
    }

    fn load_cache(&mut self) {
        let (Some(cache_path), false) = (self.cache_path.clone(), self.config.roots.is_empty())
        else {
            return;
        };
        if !platform::same_root_set(&self.config.roots, &self.config.cache_roots) {
            return;
        }
        let cache_path = self
            .config
            .cache_slot
            .map_or_else(|| cache_path.clone(), |slot| slot.path(&cache_path));
        let generation = self.scan_generation;
        // 大缓存加载可能要几秒：同样用底部进度条提示。
        self.set_scan_progress(true);
        let sender = self.events_tx.clone();
        let hwnd = SendHwnd(self.hwnd);
        let roots: Vec<PathBuf> = self.config.roots.iter().map(PathBuf::from).collect();
        let approved = self.config.approved_volumes.clone();
        let storage_guard = self.storage_guard.clone();
        std::thread::spawn(move || {
            let _storage_guard = storage_guard;
            let outcome = (|| -> Result<(Index, SystemTime), String> {
                if !approved.is_empty()
                    && platform::volume_ids(&roots).ok().as_ref() != Some(&approved)
                {
                    return Err("索引范围所在的磁盘已变化。请核对范围后重新索引。".to_string());
                }
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
        if self.hotkey {
            unsafe { UnregisterHotKey(self.hwnd, SEARCH_HOTKEY) };
            self.hotkey = false;
        }
        self.set_scan_progress(false);
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
        api::delete_font(self.search_font);
        api::delete_font(self.font);
        self.font = std::ptr::null_mut();
    }
}

fn send_event(sender: &Sender<Event>, hwnd: SendHwnd, event: Event) {
    if sender.send(event).is_ok() {
        hwnd.post(WM_APP_EVENT);
    }
}

/// 千分位分组：标题与状态栏的大数字更易读。
fn group_digits(value: usize) -> String {
    let digits = value.to_string();
    // 从末位向前每 3 位插一个逗号，最后整体反转回来。
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    let mut since_separator = 0;
    for ch in digits.chars().rev() {
        if since_separator == 3 {
            grouped.push(',');
            since_separator = 0;
        }
        grouped.push(ch);
        since_separator += 1;
    }
    grouped.chars().rev().collect()
}

fn lparam_is_null(lparam: LPARAM) -> bool {
    lparam == 0
}

/// WM_CONTEXTMENU encodes signed 16-bit screen coordinates. Only (-1, -1)
/// means keyboard invocation; negative coordinates can belong to a monitor.
fn context_menu_position(lparam: LPARAM) -> Option<(i32, i32)> {
    let x = (lparam & 0xFFFF) as i16 as i32;
    let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
    if (x, y) == (-1, -1) {
        None
    } else {
        Some((x, y))
    }
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
    fn recommended_scope_and_legacy_cache_do_not_enable_periodic_refresh() {
        let mut config: Config = serde_json::from_value(serde_json::json!({
            "roots": ["C:\\", "D:\\"],
            "cache_roots": ["D:\\", "C:\\"]
        }))
        .expect("read a legacy scope");
        assert_eq!(config.refresh_delay(), Some(Duration::from_secs(3600)));
        assert!(!config.can_resume_refresh());

        config.refresh_enabled = true;
        config.cache_roots.clear();
        assert!(
            !config.can_resume_refresh(),
            "preselected roots need a completed index"
        );
        config.cache_roots = config.roots.clone();
        assert!(
            !config.can_resume_refresh(),
            "paths alone do not authorize a timer"
        );
        config.approved_volumes = BTreeMap::from([
            ("C:\\".to_string(), "volume-c".to_string()),
            ("D:\\".to_string(), "volume-d".to_string()),
        ]);
        config.cache_slot = Some(CacheSlot::A);
        assert!(config.can_resume_refresh());
        config.refresh_minutes = Some(15);
        assert_eq!(config.refresh_delay(), Some(Duration::from_secs(900)));
        config.refresh_minutes = Some(0);
        assert!(!config.can_resume_refresh());
        config.refresh_minutes = Some(30);
        config.roots.pop();
        assert!(
            !config.can_resume_refresh(),
            "changing the scope requires another start"
        );
    }

    #[test]
    fn startup_progress_control_uses_the_registered_win32_class() {
        assert_eq!(progress_control_class(), "msctls_progress32");
    }

    #[test]
    fn rebuilding_uses_an_unreferenced_cache_file() {
        let legacy = Path::new("storage").join("index.json");
        let first = CacheSlot::A;
        let next = first.alternate();
        assert_ne!(first.path(&legacy), legacy);
        assert_ne!(first.path(&legacy), next.path(&legacy));
        assert_eq!(next.alternate(), first);
    }

    #[test]
    fn startup_cleanup_only_requests_quit_for_a_running_window() {
        let state = Cell::new(WindowState::Starting);
        assert!(!mark_window_stopped(&state));
        assert_eq!(state.get(), WindowState::Stopped);

        state.set(WindowState::Running);
        assert!(mark_window_stopped(&state));
        assert!(!mark_window_stopped(&state));
    }

    #[cfg(windows)]
    #[test]
    fn native_window_destruction_posts_quit_only_after_startup() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            PeekMessageW, PM_NOREMOVE, PM_REMOVE, WM_QUIT,
        };

        for (initial, expected) in [(WindowState::Starting, false), (WindowState::Running, true)] {
            let quit_posted = std::thread::spawn(move || {
                WINDOW_STATE.with(|state| state.set(initial));
                let mut message = MSG::default();
                let hwnd = std::ptr::null_mut();
                unsafe {
                    // Initialize the fresh thread's queue without creating a window.
                    PeekMessageW(&mut message, hwnd, 0, 0, PM_NOREMOVE);
                    window_proc(hwnd, WM_DESTROY, 0, 0);
                    PeekMessageW(&mut message, hwnd, WM_QUIT, WM_QUIT, PM_REMOVE) != 0
                        && message.message == WM_QUIT
                }
            })
            .join()
            .expect("join message-queue test");
            assert_eq!(quit_posted, expected, "{initial:?}");
        }
    }

    #[test]
    fn reentrant_borrow_is_deferred_without_losing_search_or_resize() {
        let state = RefCell::new(Some(Vec::new()));
        let mut pending = VecDeque::new();
        let result = try_with_state(&state, |outer| {
            outer.push("outer-start");
            assert!(matches!(try_with_state(&state, |_| ()), AppAccess::Busy));
            push_deferred(
                &mut pending,
                Deferred::Command {
                    id: ID_SEARCH,
                    notification: 0x0300,
                },
            );
            push_deferred(&mut pending, Deferred::Size);
            push_deferred(&mut pending, Deferred::Size);
            outer.push("outer-end");
        });
        assert!(matches!(result, AppAccess::Ready(())));
        assert_eq!(pending.len(), 2);
        assert!(matches!(
            pending.pop_front(),
            Some(Deferred::Command {
                id: ID_SEARCH,
                notification: 0x0300
            })
        ));
        assert!(matches!(pending.pop_front(), Some(Deferred::Size)));
        assert!(matches!(
            try_with_state(&state, |state| state.clone()),
            AppAccess::Ready(value) if value == ["outer-start", "outer-end"]
        ));
    }

    #[test]
    fn synchronous_minimum_size_uses_the_current_dpi_snapshot() {
        assert_eq!(min_track_size(96), (280, 100));
        assert_eq!(min_track_size(144), (420, 150));
        assert_eq!(min_track_size(192), (560, 200));
    }

    #[test]
    fn dpi_rectangle_is_copied_before_the_sender_returns() {
        let mut suggested = RECT {
            left: -100,
            top: 20,
            right: 700,
            bottom: 600,
        };
        let captured =
            unsafe { read_suggested_rect(&mut suggested as *mut RECT as LPARAM) }.unwrap();
        suggested.left = 900;
        assert_eq!(suggested.left, 900);
        assert_eq!(
            (captured.left, captured.top, captured.right, captured.bottom),
            (-100, 20, 700, 600)
        );
        assert!(unsafe { read_suggested_rect(0) }.is_none());
    }

    #[test]
    fn notification_payload_is_copied_before_the_sender_returns() {
        let mut header_token = 0_u8;
        let mut results_token = 0_u8;
        let header = (&mut header_token as *mut u8).cast();
        let results = (&mut results_token as *mut u8).cast();
        let captured = {
            let notice = NMHEADERW {
                hdr: NMHDR {
                    hwndFrom: header,
                    code: HDN_ITEMCLICK,
                    ..Default::default()
                },
                iItem: 2,
                ..Default::default()
            };
            unsafe { read_notification(&notice as *const NMHEADERW as isize, header, results) }
        };
        assert_eq!(captured, Some(Notification::HeaderClicked(2)));
        let captured = {
            let notice = NMITEMACTIVATE {
                hdr: NMHDR {
                    hwndFrom: results,
                    code: NM_DBLCLK,
                    ..Default::default()
                },
                iItem: 7,
                ..Default::default()
            };
            unsafe { read_notification(&notice as *const NMITEMACTIVATE as isize, header, results) }
        };
        assert_eq!(captured, Some(Notification::ResultDoubleClicked(7)));
        assert_eq!(unsafe { read_notification(0, header, results) }, None);
    }

    #[test]
    fn list_draw_and_data_requests_keep_the_synchronous_default() {
        use windows_sys::Win32::UI::Controls::{
            CDRF_DODEFAULT, LVN_GETDISPINFOW, LVS_OWNERDATA, LVS_OWNERDRAWFIXED, NM_CUSTOMDRAW,
        };
        assert_eq!(CDRF_DODEFAULT, 0);
        assert_eq!(
            style::RESULTS_STYLE & (LVS_OWNERDATA | LVS_OWNERDRAWFIXED),
            0
        );
        let mut result_token = 0_u8;
        let results = (&mut result_token as *mut u8).cast();
        for code in [NM_CUSTOMDRAW, LVN_GETDISPINFOW] {
            let notice = NMHDR {
                hwndFrom: results,
                code,
                ..Default::default()
            };
            assert_eq!(
                unsafe {
                    read_notification(
                        &notice as *const NMHDR as LPARAM,
                        std::ptr::null_mut(),
                        results,
                    )
                },
                None
            );
        }
    }

    #[test]
    fn context_menu_accepts_signed_secondary_monitor_coordinates() {
        fn packed(x: i16, y: i16) -> LPARAM {
            ((u32::from(y as u16) << 16) | u32::from(x as u16)) as LPARAM
        }
        assert_eq!(context_menu_position(packed(-1, -1)), None);
        assert_eq!(context_menu_position(-1), None);
        for (x, y) in [(-1200, 500), (500, -800), (-900, -400), (-1, 0), (0, -1)] {
            assert_eq!(
                context_menu_position(packed(x, y)),
                Some((i32::from(x), i32::from(y)))
            );
        }
        assert_eq!(
            context_menu_position(packed(i16::MIN, i16::MAX)),
            Some((-32768, 32767))
        );
    }

    #[test]
    fn snapshot_label_drops_the_year() {
        assert_eq!(month_day_label("2024-01-05", "08:09:10"), "01-05 08:09:10");
        // 无法解析的日期原样保留，不吞掉信息。
        assert_eq!(month_day_label("bad", "08:09:10"), "bad 08:09:10");
    }

    fn fixture() -> Vec<Entry> {
        vec![
            Entry::new(PathBuf::from("/x/b.txt"), String::new(), 10, 200, false),
            Entry::new(PathBuf::from("/x/A.txt"), String::new(), 30, 100, false),
            Entry::new(PathBuf::from("/y/c.txt"), String::new(), 20, 300, false),
        ]
    }

    fn names(rows: &[Entry]) -> Vec<&str> {
        rows.iter().map(|entry| entry.name()).collect()
    }

    fn sorted_fixture(column: usize, descending: bool) -> Vec<Entry> {
        SearchRequest {
            generation: 0,
            index: Arc::new(Index {
                entries: fixture(),
                roots: Vec::new(),
                built_at: 0,
            }),
            query: String::new(),
            kind: None,
            sort: vec![ResultSort { column, descending }],
        }
        .run()
        .expect("column search")
        .0
    }

    #[test]
    fn sort_by_name_ignores_case_in_both_directions() {
        let rows = sorted_fixture(0, false);
        assert_eq!(names(&rows), vec!["A.txt", "b.txt", "c.txt"]);
        let rows = sorted_fixture(0, true);
        assert_eq!(names(&rows), vec!["c.txt", "b.txt", "A.txt"]);
    }

    #[test]
    fn sort_by_path_orders_directories_before_names() {
        let rows = sorted_fixture(1, false);
        assert_eq!(names(&rows), vec!["A.txt", "b.txt", "c.txt"]);
        let rows = sorted_fixture(1, true);
        assert_eq!(names(&rows), vec!["c.txt", "b.txt", "A.txt"]);
    }

    #[test]
    fn sort_by_size_and_date_uses_numbers_not_text() {
        let rows = sorted_fixture(2, false);
        let sizes: Vec<u64> = rows.iter().map(|entry| entry.size).collect();
        assert_eq!(sizes, vec![10, 20, 30]);
        let rows = sorted_fixture(2, true);
        let sizes: Vec<u64> = rows.iter().map(|entry| entry.size).collect();
        assert_eq!(sizes, vec![30, 20, 10]);
        let rows = sorted_fixture(3, false);
        let stamps: Vec<i64> = rows.iter().map(|entry| entry.modified).collect();
        assert_eq!(stamps, vec![100, 200, 300]);
    }

    #[test]
    fn column_search_uses_all_matches_before_limiting_results() {
        const MATCHES: usize = RESULT_LIMIT + 17;
        let root = PathBuf::from("fixture");
        let mut entries: Vec<Entry> = (0..MATCHES)
            .map(|position| {
                let name = format!("report-{:05}.txt", MATCHES - position - 1);
                let path = root.join(format!("{position:05}")).join(&name);
                Entry::new(path, name, position as u64, position as i64, false)
            })
            .collect();
        entries.push(Entry::new(
            root.join("report.jpg"),
            "report.jpg".into(),
            u64::MAX,
            i64::MAX,
            false,
        ));
        entries.push(Entry::new(
            root.join("unrelated.txt"),
            "unrelated.txt".into(),
            u64::MAX,
            i64::MAX,
            false,
        ));
        let index = Arc::new(Index {
            entries,
            roots: vec![root],
            built_at: 0,
        });

        for (column, name_order_is_reversed) in [(0, true), (1, false), (2, false), (3, false)] {
            for descending in [false, true] {
                let request = SearchRequest {
                    generation: 0,
                    index: Arc::clone(&index),
                    query: "report".into(),
                    kind: Some("doc".into()),
                    sort: vec![ResultSort { column, descending }],
                };
                let (rows, total) = request.run().expect("column search");
                assert_eq!(total, MATCHES);
                assert_eq!(rows.len(), RESULT_LIMIT);
                for (offset, entry) in rows.iter().enumerate() {
                    let position = if descending != name_order_is_reversed {
                        MATCHES - offset - 1
                    } else {
                        offset
                    };
                    assert_eq!(
                        entry.name(),
                        format!("report-{:05}.txt", MATCHES - position - 1),
                        "column {column}, descending {descending}, row {offset}"
                    );
                }
            }
        }
    }

    #[test]
    fn group_digits_inserts_thousand_separators() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(1_532_419), "1,532,419");
    }

    #[test]
    fn changing_columns_preserves_the_previous_order_for_equal_values() {
        let mut entries = fixture();
        entries[2].size = 10;
        let request = SearchRequest {
            generation: 0,
            index: Arc::new(Index {
                entries,
                roots: Vec::new(),
                built_at: 0,
            }),
            query: String::new(),
            kind: None,
            sort: vec![
                ResultSort {
                    column: 2,
                    descending: false,
                },
                ResultSort {
                    column: 0,
                    descending: true,
                },
            ],
        };
        let (rows, _) = request.run().expect("column search");
        assert_eq!(names(&rows), vec!["c.txt", "b.txt", "A.txt"]);
    }
}
