use super::*;
use crate::content::{self, ContentIndex, Hit};
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

const APPROVAL_VERSION: u8 = 1;
const MAX_APPROVAL_BYTES: usize = 1024 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Approval {
    version: u8,
    roots: Vec<PathBuf>,
    volumes: BTreeMap<String, String>,
    slot: Option<CacheSlot>,
}

impl Approval {
    fn active(&self) -> bool {
        self.version == APPROVAL_VERSION
            && !self.roots.is_empty()
            && !self.volumes.is_empty()
            && self.slot.is_some()
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
}

struct Job {
    cancel: Arc<AtomicBool>,
    automatic: bool,
    destination: Option<PathBuf>,
}

pub(super) struct WorkResult {
    index: ContentIndex,
    approval: Option<Approval>,
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
    title: HWND,
    location: HWND,
    text: HWND,
    previous: HWND,
    next: HWND,
    original: HWND,
}

impl Controls {
    pub(super) fn all(&self) -> impl Iterator<Item = HWND> {
        [self.mode]
            .into_iter()
            .chain(self.scope())
            .chain(self.reader())
    }

    fn scope(&self) -> [HWND; 6] {
        [
            self.scope_label,
            self.scope_list,
            self.add,
            self.remove,
            self.start,
            self.stop,
        ]
    }

    fn reader(&self) -> [HWND; 6] {
        [
            self.title,
            self.location,
            self.text,
            self.previous,
            self.next,
            self.original,
        ]
    }
}

pub(super) struct State {
    pub(super) controls: Controls,
    pub(super) searching: bool,
    pub(super) index: Option<Arc<ContentIndex>>,
    pub(super) hits: Vec<Hit>,
    pub(super) query: String,
    inactive_sort: Vec<ResultSort>,
    search_notice: String,
    config: Option<PathBuf>,
    approval: Approval,
    reading: Option<Reading>,
    job: Option<Job>,
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
            hits: Vec::new(),
            query: String::new(),
            inactive_sort: Vec::new(),
            search_notice: String::new(),
            config,
            approval,
            reading: None,
            job: None,
            notice,
        }
    }

    pub(super) fn set_search_notice(&mut self, text: impl Into<String>) {
        self.search_notice = text.into();
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

    pub(super) fn cancel(&self, only_automatic: bool) {
        if let Some(job) = self
            .job
            .as_ref()
            .filter(|job| !only_automatic || job.automatic)
        {
            job.cancel.store(true, Ordering::SeqCst);
        }
    }

    pub(super) fn notice(&self, panel: Panel, shortcut: &str) -> String {
        if panel == Panel::Preview {
            return self.reading.as_ref().map_or_else(String::new, |reader| {
                format!(
                    "只读文本预览 · {}",
                    reader.index.document(&reader.hit).path.display()
                )
            });
        }
        if panel == Panel::Results && !self.search_notice.is_empty() {
            return self.search_notice.clone();
        }
        let base = if !self.notice.is_empty() {
            self.notice.clone()
        } else {
            "正文索引未启用。请选目录，再点“开始正文索引”。".into()
        };
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
        };
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
        api::enable(controls.add, !busy && body.approval.roots.len() < MAX_ROOTS);
        api::enable(controls.remove, !busy && selected >= 0);
        api::enable(controls.start, !busy && !body.approval.roots.is_empty());
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
        show(&controls.reader(), reader);
        show(&controls.scope(), settings);
        if reader {
            let locations_y = self.px(94);
            let open = self.px(112);
            let step = self.px(76);
            let buttons_width = open + 2 * step + 2 * gap;
            let wrap_location = content_width < buttons_width + self.px(166);
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
                content_width - buttons_width - gap
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
            let text_y = open_y + row + gap;
            place(
                controls.text,
                p,
                text_y,
                content_width,
                height - self.px(45) - text_y,
            );
        }
        if settings {
            let button = self.px(132);
            let list_y = self.px(92);
            let wrap_footer = content_width < 2 * button + gap;
            let footer = height - self.px(81) - if wrap_footer { row + gap } else { 0 };
            place(controls.scope_label, p, self.px(60), content_width, row);
            place(
                controls.scope_list,
                p,
                list_y,
                content_width - button - gap,
                footer - list_y - gap,
            );
            place(controls.add, right - button, list_y, button, row);
            place(
                controls.remove,
                right - button,
                list_y + row + gap,
                button,
                row,
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
            self.content.hits.clear();
            self.set_rows(Vec::new());
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
            ID_CONTENT_PREVIOUS if self.panel == Panel::Preview => self.step_preview(false),
            ID_CONTENT_NEXT if self.panel == Panel::Preview => self.step_preview(true),
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
            self.content.cancel(false);
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
        self.content.index = None;
        self.content.hits.clear();
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
        self.start_content_job("正在加载正文索引…", false, None, move |_| {
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
            })
        });
    }

    pub(super) fn begin_content_scan(&mut self, automatic: bool) {
        if self.content.job.is_some()
            || self.content.approval.roots.is_empty()
            || (automatic && !self.content.approval.active())
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
            automatic,
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
                    }),
                })
            },
        );
    }

    fn start_content_job(
        &mut self,
        notice: &str,
        automatic: bool,
        destination: Option<PathBuf>,
        task: impl FnOnce(&AtomicBool) -> Result<WorkResult, String> + Send + 'static,
    ) {
        let sender = self.events_tx.clone();
        let hwnd = SendHwnd(self.hwnd);
        let guard = self.storage_guard.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.content.job = Some(Job {
            cancel: Arc::clone(&cancel),
            automatic,
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
        self.refresh_content_controls(false);
        self.refresh_empty_state();
        self.render_status();
    }

    pub(super) fn display_content_hits(&mut self, query: String, hits: Vec<Hit>, total: usize) {
        let Some(index) = &self.content.index else {
            return;
        };
        populate_result_control(
            self.results,
            hits.iter().map(|hit| {
                let doc = index.document(hit);
                [
                    doc.name().to_string(),
                    hit.snippet.clone(),
                    format!("{} 处", hit.count),
                    doc.format.location(hit.location),
                ]
            }),
        );
        self.content.query = query;
        self.content.hits = hits;
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

    pub(super) fn open_content_hit(&mut self, position: usize) {
        if self.rows_generation != Some(self.search_generation) || self.panel != Panel::Results {
            return;
        }
        let (Some(hit), Some(index)) =
            (self.content.hits.get(position), self.content.index.clone())
        else {
            return;
        };
        let document = index.document(hit);
        match self.open_content_path(&document.path) {
            Ok(file)
                if file
                    .metadata()
                    .is_ok_and(|metadata| document.matches_metadata(&metadata)) => {}
            Ok(_) => {
                api::warn(self.hwnd, "文件已修改。请先刷新正文索引，再查看命中位置。");
                return;
            }
            Err(err) => {
                api::warn(self.hwnd, &err);
                return;
            }
        }
        self.content.reading = Some(Reading {
            index,
            hit: hit.clone(),
            term: self.content.query.clone(),
            ordinal: 0,
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
        unsafe { SetFocus(controls.text) };
        api::send_message(
            controls.text,
            style::EM_SETSEL,
            preview.selection.start,
            preview.selection.end as isize,
        );
        api::send_message(controls.text, EM_SCROLLCARET, 0, 0);
        api::enable(controls.previous, reader.ordinal > 0);
        api::enable(controls.next, reader.ordinal + 1 < reader.hit.count);
        self.render_status();
    }

    pub(super) fn clear_preview(&mut self) {
        if self.content.reading.take().is_some() {
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
}
