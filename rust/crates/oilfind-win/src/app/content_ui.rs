use super::*;
use crate::content::{self, ContentIndex, Document, Format, Hit};
use crate::pdf_preview::RenderedPage;
use crate::pdf_worker;
use std::fs::File;
use std::io::Read;
use std::sync::OnceLock;
use windows_sys::Win32::UI::Controls::EM_SCROLLCARET;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ES_MULTILINE, ES_NOHIDESEL, ES_READONLY, WS_VSCROLL,
};

pub(super) const ID_CONTENT: usize = 154;
pub(super) const ID_CONTENT_ADD: usize = 155;
pub(super) const ID_CONTENT_REMOVE: usize = 156;
pub(super) const ID_CONTENT_START: usize = 157;
pub(super) const ID_CONTENT_STOP: usize = 158;
pub(super) const ID_CONTENT_ROOTS: usize = 159;
pub(super) const ID_CONTENT_PREVIOUS: usize = 160;
pub(super) const ID_CONTENT_NEXT: usize = 161;
pub(super) const ID_CONTENT_ORIGINAL: usize = 162;
const ID_CONTENT_TEXT: usize = 163;
const ID_CONTENT_INTERVAL: usize = 164;
const ID_CONTENT_DETAILS: usize = 165;
const ID_CONTENT_PDF: usize = 166;
pub(super) const TIMER_CONTENT_REFRESH: usize = 3;

const APPROVAL_VERSION: u8 = 1;
const MAX_APPROVAL_BYTES: usize = 1024 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Approval {
    version: u8,
    roots: Vec<PathBuf>,
    volumes: BTreeMap<String, String>,
    slot: Option<CacheSlot>,
    refresh_minutes: u32,
}

impl Approval {
    fn active(&self) -> bool {
        self.version == APPROVAL_VERSION
            && !self.roots.is_empty()
            && !self.volumes.is_empty()
            && self.slot.is_some()
    }

    fn refresh_delay(&self) -> Option<Duration> {
        (self.active()
            && self.refresh_minutes > 0
            && REFRESH_PRESETS
                .iter()
                .any(|(minutes, _)| *minutes == self.refresh_minutes))
        .then(|| Duration::from_secs(u64::from(self.refresh_minutes) * 60))
    }

    fn refresh_preset(&self) -> usize {
        REFRESH_PRESETS
            .iter()
            .position(|(minutes, _)| *minutes == self.refresh_minutes)
            .unwrap_or(0)
    }

    fn contains(&self, path: &Path) -> bool {
        self.roots
            .iter()
            .any(|root| platform::within_root(root, path))
    }

    fn load(path: &Path) -> Result<Self, String> {
        let bytes = match content::read_local_file(path, MAX_APPROVAL_BYTES) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err.to_string()),
        };
        let approval: Self = serde_json::from_slice(&bytes).map_err(|err| err.to_string())?;
        if approval.version != APPROVAL_VERSION || approval.roots.len() > MAX_ROOTS {
            return Err("正文索引设置版本或范围无效".into());
        }
        Ok(approval)
    }
}

fn slot_path(config: &Path, slot: CacheSlot) -> PathBuf {
    config.with_file_name(match slot {
        CacheSlot::A => "content-index-a.json",
        CacheSlot::B => "content-index-b.json",
    })
}

struct Reading {
    index: Arc<ContentIndex>,
    hit: Hit,
    term: String,
    ordinal: usize,
    pdf: Option<Arc<PdfSource>>,
    view: ReaderView,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ReaderView {
    #[default]
    Automatic,
    Text,
    Page,
}

struct PdfSource {
    file: File,
    bytes: OnceLock<Result<Arc<Vec<u8>>, String>>,
}

impl PdfSource {
    fn bytes(&self, document: &Document) -> Result<Arc<Vec<u8>>, String> {
        self.bytes
            .get_or_init(|| {
                let limit = content::MAX_FILE_BYTES;
                let length = self.file.metadata().map_err(|err| err.to_string())?.len();
                if length > limit as u64 || !document.matches_file(&self.file) {
                    return Err("PDF 大小或版本已变化，请刷新索引。".into());
                }
                let mut bytes = Vec::new();
                (&self.file)
                    .take((limit + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|err| err.to_string())?;
                if bytes.len() as u64 != length || !document.matches_file(&self.file) {
                    return Err("PDF 在打开时发生变化，请刷新索引。".into());
                }
                Ok(Arc::new(bytes))
            })
            .clone()
    }
}

struct PdfRequest {
    ticket: u64,
    source: Arc<PdfSource>,
    index: Arc<ContentIndex>,
    hit: Hit,
    term: String,
    ordinal: usize,
}

impl PdfRequest {
    fn render(&self) -> Result<Arc<RenderedPage>, String> {
        let document = self.index.document(&self.hit);
        let (page, text, selection) = document
            .pdf_match(&self.term, self.ordinal)
            .ok_or_else(|| "无法从缓存取得 PDF 命中位置。".to_string())?;
        pdf_worker::render(self.source.bytes(document)?, page, text, selection).map(Arc::new)
    }
}

#[derive(Default)]
struct PdfJobs {
    ticket: u64,
    active: bool,
    pending: Option<PdfRequest>,
    page: Option<Arc<RenderedPage>>,
    error: Option<String>,
}

struct Job {
    cancel: Arc<AtomicBool>,
    destination: Option<PathBuf>,
}

pub(super) struct WorkResult {
    index: ContentIndex,
    approval: Option<Approval>,
    updated: Option<SystemTime>,
}

#[derive(Default)]
pub(super) struct Controls {
    pub(super) mode: HWND,
    scope_label: HWND,
    scope_list: HWND,
    add: HWND,
    remove: HWND,
    start: HWND,
    stop: HWND,
    interval_label: HWND,
    pub(super) interval: HWND,
    details: HWND,
    title: HWND,
    location: HWND,
    text: HWND,
    previous: HWND,
    next: HWND,
    original: HWND,
    pdf_toggle: HWND,
    pdf_canvas: HWND,
}

impl Controls {
    pub(super) fn all(&self) -> impl Iterator<Item = HWND> {
        [self.mode]
            .into_iter()
            .chain(self.scope())
            .chain(self.reader())
    }

    fn scope(&self) -> [HWND; 9] {
        [
            self.scope_label,
            self.scope_list,
            self.add,
            self.remove,
            self.start,
            self.stop,
            self.interval_label,
            self.interval,
            self.details,
        ]
    }

    fn reader(&self) -> [HWND; 8] {
        [
            self.title,
            self.location,
            self.text,
            self.previous,
            self.next,
            self.original,
            self.pdf_toggle,
            self.pdf_canvas,
        ]
    }
}

pub(super) struct State {
    pub(super) controls: Controls,
    pub(super) searching: bool,
    pub(super) index: Option<Arc<ContentIndex>>,
    pub(super) result_index: Option<Arc<ContentIndex>>,
    pub(super) hits: Vec<Hit>,
    pub(super) query: String,
    pub(super) row_height: result_view::RowsHeight,
    anchor: Option<result_view::Anchor>,
    inactive_sort: Vec<ResultSort>,
    search_notice: String,
    config: Option<PathBuf>,
    approval: Approval,
    reading: Option<Reading>,
    pdf: PdfJobs,
    job: Option<Job>,
    next_refresh: Option<Instant>,
    updated: Option<SystemTime>,
    notice: String,
}

impl State {
    pub(super) fn new(config: Option<&Path>) -> Self {
        let config = config.map(|path| path.with_file_name("content-config.json"));
        let loaded = config.as_deref().map(Approval::load).transpose();
        let (approval, notice) = match loaded {
            Ok(approval) => (approval.unwrap_or_default(), String::new()),
            Err(err) => (Approval::default(), format!("无法加载正文索引设置：{err}")),
        };
        Self {
            controls: Controls::default(),
            searching: false,
            index: None,
            result_index: None,
            hits: Vec::new(),
            query: String::new(),
            row_height: result_view::RowsHeight::default(),
            anchor: None,
            inactive_sort: Vec::new(),
            search_notice: String::new(),
            config,
            approval,
            reading: None,
            pdf: PdfJobs::default(),
            job: None,
            next_refresh: None,
            updated: None,
            notice,
        }
    }

    pub(super) fn set_search_notice(&mut self, text: impl Into<String>) {
        self.search_notice = text.into();
    }

    pub(super) fn clear_results(&mut self) {
        self.hits.clear();
        self.result_index = None;
        self.anchor = None;
    }

    pub(super) fn empty_message(&self) -> &'static str {
        if !self.hits.is_empty() {
            ""
        } else if self.index.is_none() && self.job.is_some() {
            "正文索引正在后台处理…"
        } else if self.index.is_none() {
            "在“索引设置 → 正文索引”里选目录并开始索引。"
        } else if self.search_notice == "正在搜正文…" {
            ""
        } else {
            "没有匹配的正文结果。"
        }
    }

    fn save(&self, approval: &Approval) -> Result<(), String> {
        if let Some(path) = &self.config {
            let bytes = serde_json::to_vec(approval).map_err(|err| err.to_string())?;
            if bytes.len() > MAX_APPROVAL_BYTES {
                return Err("正文索引设置超出大小限制".into());
            }
            api::write_configuration(path, &bytes).map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    pub(super) fn cancel(&self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::SeqCst);
        }
    }

    fn visible_pdf(&self) -> bool {
        self.reading.as_ref().is_some_and(|reader| {
            self.pdf
                .page
                .as_ref()
                .is_some_and(|page| match reader.view {
                    ReaderView::Automatic => page.highlight.is_some(),
                    ReaderView::Text => false,
                    ReaderView::Page => true,
                })
        })
    }

    pub(super) fn notice(&self, panel: Panel, shortcut: &str) -> String {
        if panel == Panel::Preview {
            return self.reading.as_ref().map_or_else(String::new, |reader| {
                let mode = if let Some(error) = &self.pdf.error {
                    return format!("PDF 原页预览不可用 · {error}");
                } else if self.visible_pdf() {
                    if self
                        .pdf
                        .page
                        .as_ref()
                        .is_some_and(|page| page.highlight.is_none())
                    {
                        "PDF 原页 · 精确命中可在文本预览查看"
                    } else {
                        "PDF 原页 · 滚动查看页面"
                    }
                } else if reader.pdf.is_some()
                    && self.pdf.page.is_none()
                    && reader.view != ReaderView::Text
                {
                    "正在渲染 PDF 原页…"
                } else if reader.pdf.is_some() && reader.view == ReaderView::Automatic {
                    "只读文本预览 · 原页位置暂无法精确标记"
                } else {
                    "只读文本预览"
                };
                format!(
                    "{mode} · {}",
                    reader.index.document(&reader.hit).path.display()
                )
            });
        }
        let mut base = if panel == Panel::Results && !self.search_notice.is_empty() {
            self.search_notice.clone()
        } else if !self.notice.is_empty() {
            self.notice.clone()
        } else {
            "正文索引未启用。请选目录，再点“开始正文索引”。".into()
        };
        if let Some(updated) = self.updated {
            base.push_str(&format!(" · 更新 {}", system_time_label(updated)));
        }
        if panel == Panel::Search {
            format!("{base} · {shortcut}")
        } else {
            base
        }
    }
}

impl App {
    pub(super) fn create_content_ui(&mut self) -> Result<(), String> {
        let make = |class, label, style, id| {
            api::create_control(
                self.hwnd,
                self.instance,
                class,
                label,
                style,
                0,
                id,
                self.font,
            )
        };
        self.content.controls = Controls {
            mode: make("BUTTON", "搜正文", style::WS_TABSTOP, ID_CONTENT)?,
            scope_label: make("STATIC", "正文索引范围", style::SS_LEFT, 0)?,
            scope_list: make(
                "LISTBOX",
                "",
                style::WS_TABSTOP | style::LBS_NOTIFY | WS_BORDER | WS_VSCROLL,
                ID_CONTENT_ROOTS,
            )?,
            add: make("BUTTON", "添加目录…", style::WS_TABSTOP, ID_CONTENT_ADD)?,
            remove: make("BUTTON", "移除范围", style::WS_TABSTOP, ID_CONTENT_REMOVE)?,
            start: make(
                "BUTTON",
                "开始正文索引",
                style::WS_TABSTOP,
                ID_CONTENT_START,
            )?,
            stop: make("BUTTON", "停用正文索引", style::WS_TABSTOP, ID_CONTENT_STOP)?,
            interval_label: make("STATIC", "刷新间隔", style::SS_LEFT, 0)?,
            interval: make(
                "COMBOBOX",
                "",
                style::WS_TABSTOP | style::CBS_DROPDOWNLIST | WS_VSCROLL,
                ID_CONTENT_INTERVAL,
            )?,
            details: make("BUTTON", "索引详情", style::WS_TABSTOP, ID_CONTENT_DETAILS)?,
            title: make("STATIC", "", style::SS_LEFT | style::SS_ENDELLIPSIS, 0)?,
            location: make("STATIC", "", style::SS_LEFT, 0)?,
            text: make(
                "EDIT",
                "",
                style::WS_TABSTOP
                    | WS_BORDER
                    | WS_VSCROLL
                    | ES_MULTILINE as u32
                    | ES_READONLY as u32
                    | ES_NOHIDESEL as u32,
                ID_CONTENT_TEXT,
            )?,
            previous: make("BUTTON", "上一处", style::WS_TABSTOP, ID_CONTENT_PREVIOUS)?,
            next: make("BUTTON", "下一处", style::WS_TABSTOP, ID_CONTENT_NEXT)?,
            original: make(
                "BUTTON",
                "打开原文件",
                style::WS_TABSTOP,
                ID_CONTENT_ORIGINAL,
            )?,
            pdf_toggle: make("BUTTON", "原页预览", style::WS_TABSTOP, ID_CONTENT_PDF)?,
            pdf_canvas: pdf_canvas::create(self.hwnd, self.instance).unwrap_or_default(),
        };
        for (_, label) in REFRESH_PRESETS {
            let label = api::wide(label);
            api::send_message(
                self.content.controls.interval,
                style::CB_ADDSTRING,
                0,
                label.as_ptr() as isize,
            );
        }
        WINDOW_CONTENT_TEXT.with(|handle| handle.set(self.content.controls.text));
        let padding = self.px(10) as isize;
        api::send_message(
            self.content.controls.text,
            style::EM_SETMARGINS,
            3,
            padding | (padding << 16),
        );
        self.refresh_content_controls(true);
        Ok(())
    }

    pub(super) fn refresh_content_controls(&self, reset: bool) {
        let body = &self.content;
        let controls = &body.controls;
        if reset {
            api::send_message(controls.scope_list, style::LB_RESETCONTENT, 0, 0);
            for root in &body.approval.roots {
                let text = api::wide(&root.to_string_lossy());
                api::send_message(
                    controls.scope_list,
                    style::LB_ADDSTRING,
                    0,
                    text.as_ptr() as isize,
                );
            }
        }
        let selected = api::send_message(controls.scope_list, style::LB_GETCURSEL, 0, 0);
        let busy = body.job.is_some();
        api::send_message(
            controls.interval,
            style::CB_SETCURSEL,
            body.approval.refresh_preset(),
            0,
        );
        api::enable(controls.interval, !busy && body.config.is_some());
        api::enable(
            controls.details,
            body.index.is_some() || !body.notice.is_empty(),
        );
        api::enable(controls.add, !busy && body.approval.roots.len() < MAX_ROOTS);
        api::enable(controls.remove, !busy && selected >= 0);
        api::enable(controls.start, !busy && !body.approval.roots.is_empty());
        api::put_text(
            controls.start,
            if body.index.is_some() {
                "立即刷新"
            } else {
                "开始正文索引"
            },
        );
        api::enable(
            controls.stop,
            busy || body.index.is_some() || body.approval.active(),
        );
        api::put_text(
            controls.stop,
            if busy {
                "取消正文索引"
            } else {
                "停用正文索引"
            },
        );
    }

    pub(super) fn layout_content(
        &self,
        show: impl Fn(&[HWND], bool),
        place: impl Fn(HWND, i32, i32, i32, i32),
        size: (i32, i32),
    ) {
        let controls = &self.content.controls;
        let (width, height) = size;
        let p = self.px(24);
        let row = self.px(34);
        let gap = self.px(10);
        let content_width = width - 2 * p;
        let right = width - p;
        let reader = self.panel == Panel::Preview;
        let settings = self.panel == Panel::ContentSettings;
        show(&[controls.mode], !reader && !settings);
        let pdf_available = reader
            && self
                .content
                .reading
                .as_ref()
                .is_some_and(|reader| reader.pdf.is_some());
        let show_pdf = reader && self.content.visible_pdf();
        show(
            &[
                controls.title,
                controls.location,
                controls.previous,
                controls.next,
                controls.original,
            ],
            reader,
        );
        show(&[controls.text], reader && !show_pdf);
        show(&[controls.pdf_toggle], pdf_available);
        if !controls.pdf_canvas.is_null() {
            show(&[controls.pdf_canvas], show_pdf);
        }
        show(&controls.scope(), settings);
        if reader {
            let locations_y = self.px(94);
            let open = self.px(112);
            let step = self.px(76);
            let buttons_width = open + 2 * step + 2 * gap;
            let toggle_width = self.px(106);
            let all_buttons = buttons_width + if pdf_available { toggle_width + gap } else { 0 };
            let wrap_toggle = pdf_available && content_width < all_buttons;
            let wrap_location = content_width < all_buttons + self.px(166);
            let buttons_y = locations_y + if wrap_location { row } else { 0 };
            let wrap_open = content_width < buttons_width;
            let open_y = buttons_y + if wrap_open { row + gap } else { 0 };
            let left_buttons = if wrap_open {
                right - 2 * step - gap
            } else {
                right - buttons_width
            };
            let location_width = if wrap_location {
                content_width
            } else {
                content_width - all_buttons - gap
            };
            place(controls.title, p, self.px(57), content_width, row);
            place(
                controls.location,
                p,
                locations_y + self.px(4),
                location_width,
                row,
            );
            place(controls.previous, left_buttons, buttons_y, step, row);
            place(
                controls.next,
                left_buttons + step + gap,
                buttons_y,
                step,
                row,
            );
            place(controls.original, right - open, open_y, open, row);
            let text_y = open_y + row + gap + if wrap_toggle { row + gap } else { 0 };
            if pdf_available {
                let (toggle_x, toggle_y) = if wrap_toggle {
                    (p, open_y + row + gap)
                } else {
                    (left_buttons - toggle_width - gap, buttons_y)
                };
                place(controls.pdf_toggle, toggle_x, toggle_y, toggle_width, row);
            }
            let surface_height = height - self.px(45) - text_y;
            place(controls.text, p, text_y, content_width, surface_height);
            if !controls.pdf_canvas.is_null() {
                place(
                    controls.pdf_canvas,
                    p,
                    text_y,
                    content_width,
                    surface_height,
                );
            }
        }
        if settings {
            let button = self.px(132);
            let list_y = self.px(92);
            let wrap_footer = content_width < 2 * button + gap;
            let footer = height - self.px(81) - if wrap_footer { row + gap } else { 0 };
            let interval_y = footer - row - gap;
            place(controls.scope_label, p, self.px(60), content_width, row);
            place(
                controls.scope_list,
                p,
                list_y,
                content_width - button - gap,
                interval_y - list_y - gap,
            );
            place(controls.add, right - button, list_y, button, row);
            place(
                controls.remove,
                right - button,
                list_y + row + gap,
                button,
                row,
            );
            place(
                controls.details,
                right - button,
                list_y + 2 * (row + gap),
                button,
                row,
            );
            place(
                controls.interval_label,
                p,
                interval_y + self.px(5),
                self.px(76),
                row,
            );
            place(
                controls.interval,
                p + self.px(82),
                interval_y,
                self.px(142).min(content_width - self.px(82)),
                self.px(230),
            );
            place(controls.start, right - button, footer, button, row);
            let (stop_x, stop_y) = if wrap_footer {
                (right - button, footer + row + gap)
            } else {
                (right - 2 * button - gap, footer)
            };
            place(controls.stop, stop_x, stop_y, button, row);
        }
    }

    pub(super) fn content_mode_button(&mut self) {
        if self.panel == Panel::Settings {
            self.position_panel(Panel::ContentSettings, false);
            unsafe { SetFocus(self.content.controls.scope_list) };
        } else {
            self.content.searching = !self.content.searching;
            self.rows_generation = None;
            self.clear_rows();
            std::mem::swap(&mut self.sort, &mut self.content.inactive_sort);
            self.refresh_header();
            let cue = if self.content.searching {
                "搜索文件正文"
            } else {
                "搜索文件名、路径或拼音"
            };
            let cue = api::wide(cue);
            api::send_message(
                self.search,
                style::EM_SETCUEBANNER,
                0,
                cue.as_ptr() as isize,
            );
            self.update_content_button();
            self.layout();
            self.search_now();
            unsafe { SetFocus(self.search) };
        }
        self.render_status();
    }

    pub(super) fn update_content_button(&self) {
        let text = match self.panel {
            Panel::Settings => "正文索引",
            _ if self.content.searching => "搜文件名",
            _ => "搜正文",
        };
        api::put_text(self.content.controls.mode, text);
    }

    pub(super) fn content_command(&mut self, id: usize, notification: u32) {
        if id == ID_CONTENT_ROOTS && notification == 1 {
            self.refresh_content_controls(false);
        }
        if id == ID_CONTENT_INTERVAL && notification == 1 && self.panel == Panel::ContentSettings {
            self.change_content_interval();
        }
        if notification != 0 {
            return;
        }
        match id {
            ID_CONTENT => self.content_mode_button(),
            ID_CONTENT_ADD if self.panel == Panel::ContentSettings => {
                self.change_content_scope(true)
            }
            ID_CONTENT_REMOVE if self.panel == Panel::ContentSettings => {
                self.change_content_scope(false)
            }
            ID_CONTENT_START if self.panel == Panel::ContentSettings => {
                self.begin_content_scan(false)
            }
            ID_CONTENT_STOP if self.panel == Panel::ContentSettings => self.stop_content(),
            ID_CONTENT_DETAILS if self.panel == Panel::ContentSettings => {
                self.show_content_details()
            }
            ID_CONTENT_PREVIOUS if self.panel == Panel::Preview => self.step_preview(false),
            ID_CONTENT_NEXT if self.panel == Panel::Preview => self.step_preview(true),
            ID_CONTENT_PDF if self.panel == Panel::Preview => self.toggle_pdf_view(),
            ID_CONTENT_ORIGINAL if self.panel == Panel::Preview => {
                if let Some(reader) = &self.content.reading {
                    self.content_file_action(&reader.index.document(&reader.hit).path, ID_OPEN);
                }
            }
            _ => {}
        }
    }

    fn change_content_scope(&mut self, add: bool) {
        if self.content.job.is_some() {
            return;
        }
        let mut roots = self.content.approval.roots.clone();
        if add {
            let selected = match api::choose_folder(self.hwnd) {
                Ok(path) if path.is_empty() => return,
                Ok(path) => path,
                Err(err) => {
                    api::warn(self.hwnd, &err);
                    return;
                }
            };
            let root = match platform::local_directory(Path::new(&selected)) {
                Ok(path) => path,
                Err(err) => {
                    api::warn(self.hwnd, &err);
                    return;
                }
            };
            if self.content.approval.contains(&root) {
                return;
            }
            let candidate = match oilfind_core::validate_scan_roots(std::slice::from_ref(&root)) {
                Ok(mut roots) => roots.remove(0),
                Err(err) => {
                    api::warn(self.hwnd, &err.to_string());
                    return;
                }
            };
            roots.retain(|root| !platform::within_root(&candidate, root));
            roots.push(candidate);
        } else {
            let position =
                api::send_message(self.content.controls.scope_list, style::LB_GETCURSEL, 0, 0);
            if position < 0 || position as usize >= roots.len() {
                return;
            }
            roots.remove(position as usize);
        }
        self.save_pending_scope(roots, "范围已更新。请点击“开始正文索引”。");
    }

    fn stop_content(&mut self) {
        if self.content.job.is_some() {
            self.content.cancel();
            api::enable(self.content.controls.stop, false);
            self.content.notice = "正在取消正文索引…".into();
            self.render_status();
        } else {
            self.save_pending_scope(self.content.approval.roots.clone(), "正文索引已停用。");
        }
    }

    fn save_pending_scope(&mut self, roots: Vec<PathBuf>, note: &str) {
        let approval = Approval {
            version: APPROVAL_VERSION,
            roots,
            refresh_minutes: self.content.approval.refresh_minutes,
            ..Default::default()
        };
        if let Err(err) = self.content.save(&approval) {
            api::warn(
                self.hwnd,
                &format!("正文索引设置保存失败，仍使用上次保存的范围：{err}"),
            );
            return;
        }
        self.content.approval = approval;
        self.disarm_content_refresh();
        self.content.index = None;
        self.content.updated = None;
        self.content.clear_results();
        self.content.search_notice.clear();
        if self.content.searching {
            self.rows_generation = None;
            self.search_generation += 1;
            self.clear_rows();
        }
        self.content.notice = note.to_string();
        self.clean_content_slots();
        self.refresh_content_controls(true);
        self.render_status();
    }

    fn change_content_interval(&mut self) {
        let selected = api::send_message(self.content.controls.interval, style::CB_GETCURSEL, 0, 0);
        let Some(&(refresh_minutes, _)) = REFRESH_PRESETS.get(selected as usize) else {
            return;
        };
        if self.content.job.is_some() || self.content.approval.refresh_minutes == refresh_minutes {
            return;
        }
        let approval = Approval {
            version: APPROVAL_VERSION,
            refresh_minutes,
            ..self.content.approval.clone()
        };
        match self.content.save(&approval) {
            Ok(()) => {
                self.content.approval = approval;
                self.schedule_content_refresh();
            }
            Err(err) => api::warn(self.hwnd, &format!("无法保存正文刷新间隔：{err}")),
        }
        self.refresh_content_controls(false);
        self.render_status();
    }

    fn show_content_details(&self) {
        let state = &self.content;
        let updated = state
            .updated
            .map(system_time_label)
            .unwrap_or_else(|| "暂无记录".into());
        let mut lines = vec![format!("上次成功更新  {updated}")];
        if let Some(index) = &state.index {
            lines.push(format!("已索引  {} 份文件", group_digits(index.len())));
            if let Some(report) = index.report() {
                lines.extend([
                    format!("复用未修改正文  {}", group_digits(report.reused)),
                    format!("提取新增或修改正文  {}", group_digits(report.extracted)),
                    String::new(),
                    format!(
                        "无可搜索文字（含扫描版 PDF）  {}",
                        group_digits(report.no_text)
                    ),
                    format!("文件或索引超出上限  {}", group_digits(report.too_large)),
                    format!("被占用或无读取权限  {}", group_digits(report.inaccessible)),
                    format!("正文提取失败  {}", group_digits(report.extraction_failed)),
                    format!(
                        "文件已移除或无法确认位置  {}",
                        group_digits(report.unavailable)
                    ),
                ]);
            } else {
                lines.push(format!(
                    "跳过 {} 份文件。刷新一次即可看到分类。",
                    group_digits(index.skipped)
                ));
            }
        }
        if !state.notice.is_empty() {
            lines.extend([String::new(), state.notice.clone()]);
        }
        api::info(self.hwnd, &lines.join("\n"));
    }

    pub(super) fn disarm_content_refresh(&mut self) {
        unsafe { KillTimer(self.hwnd, TIMER_CONTENT_REFRESH) };
        self.content.next_refresh = None;
    }

    fn schedule_content_refresh(&mut self) {
        self.disarm_content_refresh();
        let Some(delay) = self
            .content
            .approval
            .refresh_delay()
            .filter(|_| self.content.index.is_some() && self.content.job.is_none())
        else {
            return;
        };
        let deadline = Instant::now() + delay;
        self.arm_content_refresh(deadline, delay);
    }

    fn arm_content_refresh(&mut self, deadline: Instant, delay: Duration) {
        let millis = (delay.as_millis() + 1).min(u128::from(u32::MAX)) as u32;
        if unsafe { SetTimer(self.hwnd, TIMER_CONTENT_REFRESH, millis, None) } == 0 {
            self.disarm_content_refresh();
            self.content
                .notice
                .push_str(" 无法安排定时刷新，可手动刷新。");
            self.render_status();
            return;
        }
        self.content.next_refresh = Some(deadline);
    }

    pub(super) fn refresh_content_if_due(&mut self) {
        let Some(due) = self.content.next_refresh else {
            return;
        };
        if let Some(remaining) = due
            .checked_duration_since(Instant::now())
            .filter(|delay| !delay.is_zero())
        {
            self.arm_content_refresh(due, remaining);
            return;
        }
        self.disarm_content_refresh();
        self.begin_content_scan(true);
    }

    fn clean_content_slots(&mut self) {
        let Some(config) = &self.content.config else {
            return;
        };
        for slot in [CacheSlot::A, CacheSlot::B] {
            if Some(slot) == self.content.approval.slot {
                continue;
            }
            let path = slot_path(config, slot);
            if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file())
                && !platform::is_link_or_reparse(&path)
                && std::fs::remove_file(&path).is_err()
            {
                self.content.notice.push_str(" 旧正文缓存清理失败。");
                break;
            }
        }
    }

    pub(super) fn load_content_cache(&mut self) {
        let (Some(path), Some(slot)) = (&self.content.config, self.content.approval.slot) else {
            return;
        };
        if !self.content.approval.active() {
            return;
        }
        let path = slot_path(path, slot);
        let approval = self.content.approval.clone();
        self.start_content_job("正在加载正文索引…", None, move |_| {
            let volumes = checked_volumes(&approval.roots)?;
            if volumes != approval.volumes {
                return Err("正文范围所在的磁盘已变化，请重新确认范围".into());
            }
            let data = content::read_local_file(&path, content::MAX_CACHE_BYTES)
                .map_err(|err| err.to_string())?;
            let index = ContentIndex::from_cache(&data, &approval.roots)?;
            Ok(WorkResult {
                index,
                approval: None,
                updated: std::fs::symlink_metadata(&path)
                    .ok()
                    .filter(|metadata| metadata.is_file())
                    .and_then(|metadata| metadata.modified().ok()),
            })
        });
    }

    pub(super) fn begin_content_scan(&mut self, automatic: bool) {
        if self.content.job.is_some()
            || self.content.approval.roots.is_empty()
            || (automatic && self.content.approval.refresh_delay().is_none())
        {
            return;
        }
        let scope = self.content.approval.clone();
        let previous = self.content.index.clone();
        let slot = self
            .content
            .config
            .as_ref()
            .map(|_| scope.slot.map_or(CacheSlot::A, CacheSlot::alternate));
        let destination = slot.and_then(|slot| {
            self.content
                .config
                .as_ref()
                .map(|path| slot_path(path, slot))
        });
        self.start_content_job(
            "正在建立正文索引…",
            destination.clone(),
            move |cancel| {
                let volumes = checked_volumes(&scope.roots)?;
                if automatic && volumes != scope.volumes {
                    return Err("正文范围所在的磁盘已变化，请重新确认范围".into());
                }
                let previous = previous.as_deref().filter(|_| scope.volumes == volumes);
                let index = ContentIndex::refresh(cancel, &scope.roots, &volumes, previous)?;
                if checked_volumes(&scope.roots)? != volumes {
                    return Err("正文范围在扫描期间发生变化".into());
                }
                if let Some(path) = destination {
                    let bytes = serde_json::to_vec(&index).map_err(|err| err.to_string())?;
                    if bytes.len() > content::MAX_CACHE_BYTES {
                        return Err("正文索引缓存超出大小限制".into());
                    }
                    if cancel.load(Ordering::Relaxed) {
                        return Err("正文索引已取消".into());
                    }
                    api::write_configuration(&path, &bytes).map_err(|err| err.to_string())?;
                }
                Ok(WorkResult {
                    index,
                    approval: Some(Approval {
                        version: APPROVAL_VERSION,
                        roots: scope.roots,
                        volumes,
                        slot,
                        refresh_minutes: scope.refresh_minutes,
                    }),
                    updated: Some(SystemTime::now()),
                })
            },
        );
    }

    fn start_content_job(
        &mut self,
        notice: &str,
        destination: Option<PathBuf>,
        task: impl FnOnce(&AtomicBool) -> Result<WorkResult, String> + Send + 'static,
    ) {
        let sender = self.events_tx.clone();
        let hwnd = SendHwnd(self.hwnd);
        let guard = self.storage_guard.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.disarm_content_refresh();
        self.content.job = Some(Job {
            cancel: Arc::clone(&cancel),
            destination,
        });
        self.content.notice = notice.to_string();
        self.refresh_content_controls(false);
        self.refresh_empty_state();
        self.render_status();
        std::thread::spawn(move || {
            let _guard = guard;
            let outcome = task(&cancel);
            send_event(&sender, hwnd, Event::ContentReady(outcome));
        });
    }

    pub(super) fn finish_content_job(&mut self, outcome: Result<WorkResult, String>) {
        let Some(job) = self.content.job.take() else {
            return;
        };
        let outcome = if job.cancel.load(Ordering::SeqCst) {
            Err("已取消正文索引，保留此前可用的结果。".into())
        } else {
            outcome.and_then(|result| {
                if let Some(approval) = &result.approval {
                    self.content.save(approval)?;
                }
                Ok(result)
            })
        };
        match outcome {
            Ok(result) => {
                self.content.updated = result.updated;
                self.content.notice =
                    format!("正文索引 {} 份文件", group_digits(result.index.len()));
                if result.index.skipped > 0 {
                    self.content.notice.push_str(&format!(
                        " · 跳过 {} 份无法读取或超限的文件",
                        group_digits(result.index.skipped)
                    ));
                }
                self.content.index = Some(Arc::new(result.index));
                if let Some(approval) = result.approval {
                    self.content.approval = approval;
                }
                self.clean_content_slots();
                if self.content.searching && self.panel != Panel::Preview {
                    self.search_now();
                }
            }
            Err(err) => {
                self.content.notice = format!("正文索引未更新 · {err}");
                if job.destination.is_some() {
                    self.clean_content_slots();
                }
            }
        }
        self.schedule_content_refresh();
        self.refresh_content_controls(false);
        self.refresh_empty_state();
        self.render_status();
    }

    pub(super) fn display_content_hits(&mut self, query: String, hits: Vec<Hit>, total: usize) {
        let Some(index) = self.content.index.clone() else {
            return;
        };
        let anchor = self.content.anchor.take().or_else(|| {
            self.selected_position()
                .and_then(|position| self.body_result_anchor(position))
        });
        let values: Vec<_> = hits
            .iter()
            .map(|hit| {
                let doc = index.document(hit);
                [
                    doc.name().to_string(),
                    hit.snippet.clone(),
                    format!("{} 处", hit.count),
                    doc.format.location(hit.location),
                ]
            })
            .collect();
        result_view::publish(&values, &query);
        populate_result_control(self.results, values.into_iter());
        if let Some(anchor) = anchor.filter(|_| query == self.content.query) {
            let (selection, top) =
                anchor.indices(hits.iter().map(|hit| index.document(hit).path.as_path()));
            result_view::restore(self.results, selection, top);
        }
        self.content.query = query;
        self.content.hits = hits;
        self.content.result_index = Some(index);
        let mut notice = format!("共命中 {} 份文件", group_digits(total));
        if total > RESULT_LIMIT {
            notice.push_str(&format!(" · 显示前 {RESULT_LIMIT} 份"));
        }
        if total != 0 {
            notice.push_str(" · 点击或回车查看原文");
        }
        self.content.set_search_notice(notice);
        self.refresh_empty_state();
        self.render_status();
    }

    fn body_result_anchor(&self, selection: usize) -> Option<result_view::Anchor> {
        let index = self.content.result_index.as_ref()?;
        let hit = self.content.hits.get(selection)?;
        let top = self.content.hits.get(result_view::top_index(self.results));
        Some(result_view::Anchor {
            selected: index.document(hit).path.clone(),
            top: top.map(|hit| index.document(hit).path.clone()),
        })
    }

    pub(super) fn body_results_current(&self) -> bool {
        self.rows_generation == Some(self.search_generation)
            && self
                .content
                .index
                .as_ref()
                .zip(self.content.result_index.as_ref())
                .is_some_and(|(current, listed)| Arc::ptr_eq(current, listed))
            && self.content.query == api::text(self.search).trim()
    }

    pub(super) fn return_to_body_results(&mut self, follow_pointer: bool) {
        self.position_panel(Panel::Results, follow_pointer);
        unsafe { SetFocus(self.results) };
        if !self.body_results_current() {
            self.search_now();
            return;
        }
        if let (Some(anchor), Some(index)) =
            (self.content.anchor.take(), &self.content.result_index)
        {
            let (selection, top) = anchor.indices(
                self.content
                    .hits
                    .iter()
                    .map(|hit| index.document(hit).path.as_path()),
            );
            result_view::restore(self.results, selection, top);
        }
    }

    pub(super) fn open_content_hit(&mut self, position: usize) {
        if self.rows_generation != Some(self.search_generation) || self.panel != Panel::Results {
            return;
        }
        let (Some(hit), Some(index)) = (
            self.content.hits.get(position),
            self.content.result_index.clone(),
        ) else {
            return;
        };
        let document = index.document(hit);
        let file = match self.open_content_path(&document.path) {
            Ok(file) if document.matches_file(&file) => file,
            Ok(_) => {
                api::warn(self.hwnd, "文件已修改。请先刷新正文索引，再查看命中位置。");
                return;
            }
            Err(err) => {
                api::warn(self.hwnd, &err);
                return;
            }
        };
        let pdf = (document.format == Format::Pdf && !self.content.controls.pdf_canvas.is_null())
            .then(|| {
                Arc::new(PdfSource {
                    file,
                    bytes: OnceLock::new(),
                })
            });
        self.content.anchor = self.body_result_anchor(position);
        self.content.reading = Some(Reading {
            index,
            hit: hit.clone(),
            term: self.content.query.clone(),
            ordinal: 0,
            pdf,
            view: ReaderView::default(),
        });
        self.position_panel(Panel::Preview, false);
        self.show_preview();
    }

    fn step_preview(&mut self, forward: bool) {
        if let Some(reader) = &mut self.content.reading {
            reader.ordinal = if forward {
                (reader.ordinal + 1).min(reader.hit.count - 1)
            } else {
                reader.ordinal.saturating_sub(1)
            };
        }
        self.show_preview();
    }

    fn show_preview(&mut self) {
        self.reset_pdf_page();
        let Some(reader) = &self.content.reading else {
            return;
        };
        let document = reader.index.document(&reader.hit);
        let Some(preview) = document.preview(&reader.term, reader.ordinal) else {
            return;
        };
        let controls = &self.content.controls;
        api::put_text(controls.title, document.name());
        api::put_text(
            controls.location,
            &format!(
                "{}  ·  {} / {} 处",
                document.format.location(preview.location),
                reader.ordinal + 1,
                reader.hit.count
            ),
        );
        api::put_text(controls.text, &preview.text);
        api::send_message(
            controls.text,
            style::EM_SETSEL,
            preview.selection.start,
            preview.selection.end as isize,
        );
        api::enable(controls.previous, reader.ordinal > 0);
        api::enable(controls.next, reader.ordinal + 1 < reader.hit.count);
        if let Some(source) = reader
            .pdf
            .as_ref()
            .filter(|_| reader.view != ReaderView::Text)
        {
            self.content.pdf.pending = Some(PdfRequest {
                ticket: self.content.pdf.ticket,
                source: Arc::clone(source),
                index: Arc::clone(&reader.index),
                hit: reader.hit.clone(),
                term: reader.term.clone(),
                ordinal: reader.ordinal,
            });
        }
        self.update_pdf_controls();
        unsafe { SetFocus(self.content.controls.text) };
        api::send_message(self.content.controls.text, EM_SCROLLCARET, 0, 0);
        self.start_pending_pdf();
        self.render_status();
    }

    fn reset_pdf_page(&mut self) {
        self.content.pdf.ticket = self.content.pdf.ticket.wrapping_add(1);
        self.content.pdf.pending = None;
        self.content.pdf.page = None;
        self.content.pdf.error = None;
        pdf_canvas::set_page(self.content.controls.pdf_canvas, None);
    }

    fn start_pending_pdf(&mut self) {
        let job = &mut self.content.pdf;
        if job.active {
            return;
        }
        let Some(request) = job.pending.take() else {
            return;
        };
        job.active = true;
        let sender = self.events_tx.clone();
        let hwnd = SendHwnd(self.hwnd);
        std::thread::spawn(move || {
            let result = request.render();
            send_event(
                &sender,
                hwnd,
                Event::PdfReady {
                    ticket: request.ticket,
                    result,
                },
            );
        });
    }

    pub(super) fn finish_pdf_render(
        &mut self,
        ticket: u64,
        result: Result<Arc<RenderedPage>, String>,
    ) {
        self.content.pdf.active = false;
        if self.panel == Panel::Preview && self.content.pdf.ticket == ticket {
            match result {
                Ok(page) => {
                    pdf_canvas::set_page(self.content.controls.pdf_canvas, Some(Arc::clone(&page)));
                    self.content.pdf.page = Some(page);
                }
                Err(error) => self.content.pdf.error = Some(error),
            }
            self.update_pdf_controls();
            self.render_status();
        }
        self.start_pending_pdf();
    }

    fn toggle_pdf_view(&mut self) {
        let visible = self.content.visible_pdf();
        let Some(reader) = &mut self.content.reading else {
            return;
        };
        reader.view = if visible {
            ReaderView::Text
        } else {
            ReaderView::Page
        };
        if self.content.pdf.page.is_none() {
            self.show_preview();
            return;
        }
        self.update_pdf_controls();
        self.render_status();
    }

    fn update_pdf_controls(&mut self) {
        let controls = &self.content.controls;
        let visible = self.content.visible_pdf();
        let focus = unsafe { GetFocus() };
        let next_focus = if visible {
            controls.pdf_canvas
        } else {
            controls.text
        };
        api::enable(
            controls.pdf_toggle,
            self.content.pdf.page.is_some()
                || self
                    .content
                    .reading
                    .as_ref()
                    .is_some_and(|reader| reader.pdf.is_some() && reader.view == ReaderView::Text),
        );
        api::put_text(
            controls.pdf_toggle,
            if visible {
                "文本预览"
            } else {
                "原页预览"
            },
        );
        if !self.content.controls.pdf_canvas.is_null() {
            self.layout();
        }
        if [
            self.content.controls.text,
            self.content.controls.pdf_canvas,
            self.content.controls.pdf_toggle,
        ]
        .contains(&focus)
        {
            unsafe { SetFocus(next_focus) };
        }
    }

    pub(super) fn clear_preview(&mut self) {
        if self.content.reading.take().is_some() {
            self.reset_pdf_page();
            api::put_text(self.content.controls.text, "");
        }
    }

    fn open_content_path(&self, path: &Path) -> Result<std::fs::File, String> {
        let approval = &self.content.approval;
        if !approval.roots.iter().any(|root| path.starts_with(root))
            || !platform::volume_ids(&[path.to_path_buf()]).is_ok_and(|actual| {
                actual
                    .iter()
                    .all(|(drive, id)| approval.volumes.get(drive) == Some(id))
            })
        {
            return Err("此文件的正文索引授权已变化，请重新索引。".into());
        }
        content::open_indexed_file(path, &approval.volumes).map_err(|err| err.to_string())
    }

    pub(super) fn content_file_action(&self, path: &Path, id: usize) {
        let result = self.open_content_path(path).and_then(|_file| match id {
            ID_OPEN | ID_CONTENT_ORIGINAL => api::open_path(self.hwnd, path),
            ID_REVEAL => api::reveal_path(path),
            _ => api::copy_to_clipboard(self.hwnd, &path.to_string_lossy()),
        });
        if let Err(err) = result {
            api::warn(self.hwnd, &err);
        }
    }
}

fn checked_volumes(roots: &[PathBuf]) -> Result<BTreeMap<String, String>, String> {
    for root in roots {
        let path = platform::local_directory(root)?;
        if !platform::within_root(&path, root) || !platform::within_root(root, &path) {
            return Err("正文范围实际位置已变化".into());
        }
    }
    oilfind_core::validate_scan_roots(roots).map_err(|err| err.to_string())?;
    platform::volume_ids(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selecting_roots_and_upgrading_metadata_settings_do_not_authorize_body_reads() {
        let metadata_config =
            r#"{"roots":["D:\\Reports"],"refresh_enabled":true,"cache_slot":"A"}"#;
        let legacy: Approval = serde_json::from_str(metadata_config).unwrap();
        assert!(!legacy.active());
        let mut pending = Approval {
            version: APPROVAL_VERSION,
            roots: vec![PathBuf::from(r"D:\Reports")],
            ..Default::default()
        };
        assert!(!pending.active());
        pending.volumes.insert("D:\\".into(), "volume-id".into());
        assert!(!pending.active());
        pending.slot = Some(CacheSlot::A);
        assert!(pending.active());
    }

    #[test]
    fn body_refresh_needs_its_own_persisted_interval_and_approved_snapshot() {
        let mut scope = Approval {
            version: APPROVAL_VERSION,
            roots: vec![PathBuf::from(r"D:\Reports")],
            volumes: BTreeMap::from([("D:\\".into(), "volume-id".into())]),
            slot: Some(CacheSlot::A),
            ..Default::default()
        };
        assert_eq!(scope.refresh_delay(), None);
        scope.refresh_minutes = 30;
        assert_eq!(scope.refresh_delay(), Some(Duration::from_secs(1800)));
        scope.slot = None;
        assert_eq!(scope.refresh_delay(), None);
    }
}
