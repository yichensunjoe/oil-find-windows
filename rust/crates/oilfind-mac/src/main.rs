//! macOS 预览界面：用 egui 把 `oilfind-core` 的索引/搜索能力显示出来。
//!
//! 这不是最终交付形态——Windows 客户端用的是原生 Win32 控件；这里的目的只是让
//! Rust 引擎能在 macOS 上直接试用（选目录、建索引、搜索、打开/定位/复制）。
//! 因此刻意不引入 WebView，也不做后台常驻、全局快捷键等平台集成。

use eframe::egui;
use oilfind_core::{Entry, Error, Index, Progress, MAX_ROOTS};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

mod safe_config;
#[cfg(test)]
mod tests;

const KINDS: [(&str, &str); 8] = [
    ("全部类型", ""),
    ("仅文件", "file"),
    ("仅文件夹", "folder"),
    ("文档", "doc"),
    ("图片", "image"),
    ("视频", "video"),
    ("音频", "audio"),
    ("压缩包", "archive"),
];

const RESULT_LIMIT: usize = 2000;
const CONFIG_VERSION: u32 = 1;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 660.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("Oil Find · Rust 预览"),
        ..Default::default()
    };
    eframe::run_native(
        "Oil Find",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

enum ScanMsg {
    Progress(Progress),
    Done(Result<Index, Error>),
}

struct App {
    query: String,
    kind: usize,
    roots: Vec<PathBuf>,
    cache_roots: Vec<PathBuf>,
    index: Option<Arc<Index>>,
    rows: Vec<Entry>,
    total: usize,
    selected: Option<usize>,
    status: String,
    notice: String,
    scanning: bool,
    cancel: Arc<AtomicBool>,
    rx: Option<Receiver<ScanMsg>>,
    progress: Progress,
    dirty: bool,
    focused_once: bool,
    config_path: Option<PathBuf>,
    cache_path: Option<PathBuf>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        let (config_path, cache_path) = storage_paths();
        Self::from_storage(config_path, cache_path)
    }

    fn from_storage(config_path: Option<PathBuf>, cache_path: Option<PathBuf>) -> Self {
        let mut app = App {
            query: String::new(),
            kind: 0,
            roots: Vec::new(),
            cache_roots: Vec::new(),
            index: None,
            rows: Vec::new(),
            total: 0,
            selected: None,
            status: "尚未建立索引：添加目录后点“开始索引”。".into(),
            notice: String::new(),
            scanning: false,
            cancel: Arc::new(AtomicBool::new(false)),
            rx: None,
            progress: Progress {
                visited: 0,
                skipped: 0,
                current: PathBuf::new(),
            },
            dirty: false,
            focused_once: false,
            config_path,
            cache_path,
        };
        app.load_state();
        app
    }

    fn load_state(&mut self) {
        let Some(config_path) = self.config_path.clone() else {
            self.notice = "配置目录不可用：本次仅使用内存。".into();
            return;
        };
        match safe_config::read(&config_path) {
            Ok(data) => match serde_json::from_slice::<Config>(&data) {
                Ok(config) if config.version != CONFIG_VERSION => {
                    self.notice = "旧配置无法确认授权，请重新选择目录并点“开始索引”。".into();
                    return;
                }
                Ok(config) if config.roots.len() > MAX_ROOTS => {
                    self.notice = "目录配置超出安全上限，不会自动扫描。".into();
                    return;
                }
                Ok(config) => {
                    for root in config.roots {
                        if let Some(root) = clean_root(&root) {
                            insert_root(&mut self.roots, root);
                        } else {
                            self.notice = "已跳过不可访问或包含链接的目录，请重新选择。".into();
                        }
                    }
                    self.cache_roots = config.cache_roots;
                }
                Err(err) => self.notice = format!("配置损坏，不会自动扫描：{err}"),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => self.notice = format!("读取配置失败，不会自动扫描：{err}"),
        }
        let Some(cache_path) = self.cache_path.clone() else {
            return;
        };
        if self.roots.is_empty() {
            return;
        }
        if !same_roots(&self.cache_roots, &self.roots) {
            self.notice = "当前范围没有已授权缓存，请点“开始索引”。".into();
            return;
        }
        if !cache_path.is_file() {
            return;
        }
        match Index::load(&cache_path) {
            Ok(index) if same_roots(&index.roots, &self.roots) => {
                self.status = format!("已加载缓存：{} 项（未重新扫描）。", index.entries.len());
                self.index = Some(Arc::new(index));
                self.run_search();
            }
            Ok(_) => self.notice = "缓存与当前目录范围不一致，已忽略。".into(),
            Err(err) => self.notice = format!("未加载缓存：{err}"),
        }
    }

    fn save_config(&mut self) {
        let Some(path) = self.config_path.as_ref() else {
            return;
        };
        let config = Config {
            version: CONFIG_VERSION,
            roots: self.roots.clone(),
            cache_roots: self.cache_roots.clone(),
        };
        let result = serde_json::to_vec_pretty(&config)
            .map_err(|err| err.to_string())
            .and_then(|data| safe_config::save(path, &data).map_err(|err| err.to_string()));
        if let Err(err) = result {
            self.notice = format!("保存目录配置失败：{err}");
        }
    }

    fn add_root(&mut self) {
        let Some(folder) = rfd::FileDialog::new()
            .set_title("选择要索引的目录")
            .pick_folder()
        else {
            return;
        };
        self.add_selected_root(folder);
    }

    fn add_selected_root(&mut self, folder: PathBuf) {
        let Some(folder) = clean_root(&folder) else {
            self.notice = "请选择可访问且不包含链接的本机目录。".into();
            return;
        };
        let mut roots = self.roots.clone();
        if !insert_root(&mut roots, folder) {
            self.notice = "该目录已包含在当前范围内。".into();
            return;
        }
        if roots.len() > MAX_ROOTS {
            self.notice = format!("最多 {MAX_ROOTS} 个目录。");
            return;
        }
        self.roots = roots;
        self.cache_roots.clear();
        self.notice.clear();
        self.save_config();
        self.invalidate("已添加目录，请点“开始索引”。");
    }

    fn remove_root(&mut self, position: usize) {
        if position >= self.roots.len() {
            return;
        }
        self.roots.remove(position);
        self.cache_roots.clear();
        self.notice.clear();
        self.save_config();
        self.invalidate("已删除目录（不会删除任何文件），请重新索引。");
    }

    fn invalidate(&mut self, message: &str) {
        self.stop_scan();
        self.index = None;
        self.rows.clear();
        self.total = 0;
        self.selected = None;
        self.status = message.into();
    }

    fn start_scan(&mut self) {
        if self.scanning || self.roots.is_empty() {
            return;
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.cancel);
        let roots = self.roots.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.scanning = true;
        self.progress = Progress {
            visited: 0,
            skipped: 0,
            current: PathBuf::new(),
        };
        self.status = "正在建立索引…".into();

        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let result = oilfind_core::scan(&cancel, &roots, move |progress| {
                let _ = progress_tx.send(ScanMsg::Progress(progress));
            });
            let _ = tx.send(ScanMsg::Done(result));
        });
    }

    fn stop_scan(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.scanning = false;
        self.rx = None;
    }

    fn poll_scan(&mut self) {
        let Some(rx) = &self.rx else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok(ScanMsg::Progress(progress)) => self.progress = progress,
                Ok(ScanMsg::Done(result)) => {
                    self.scanning = false;
                    self.rx = None;
                    match result {
                        Ok(index) => {
                            let count = index.entries.len();
                            let cache_roots = index.roots.clone();
                            let saved = self
                                .cache_path
                                .as_ref()
                                .map(|path| index.save_cancellable(&self.cancel, path));
                            self.index = Some(Arc::new(index));
                            self.status = match &saved {
                                Some(Err(err)) => {
                                    format!("索引完成：{count} 项；缓存未保存：{err}")
                                }
                                _ => format!("索引完成：{count} 项。"),
                            };
                            self.notice.clear();
                            self.run_search();
                            if let Some(Ok(())) = saved {
                                self.cache_roots = if same_roots(&cache_roots, &self.roots) {
                                    cache_roots
                                } else {
                                    Vec::new()
                                };
                                self.save_config();
                            }
                        }
                        Err(Error::Cancelled) => self.status = "已取消索引，保留原状态。".into(),
                        Err(err) => self.status = format!("索引失败：{err}"),
                    }
                    return;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.scanning = false;
                    self.rx = None;
                    return;
                }
            }
        }
    }

    fn run_search(&mut self) {
        let Some(index) = self.index.clone() else {
            self.rows.clear();
            self.total = 0;
            return;
        };
        let kind = KINDS[self.kind].1;
        let kind = if kind.is_empty() { None } else { Some(kind) };
        match index.search(&self.query, kind, RESULT_LIMIT) {
            Ok((rows, total)) => {
                self.rows = rows;
                self.total = total;
                self.selected = if self.rows.is_empty() { None } else { Some(0) };
                self.notice.clear();
            }
            Err(err) => {
                self.rows.clear();
                self.total = 0;
                self.notice = format!("查询无效：{err}");
            }
        }
    }

    fn action(&mut self, open: bool) {
        let Some(entry) = self.selected.and_then(|i| self.rows.get(i)).cloned() else {
            return;
        };
        let result = if open {
            std::process::Command::new("open")
                .arg(entry.path())
                .spawn()
                .map(|_| ())
        } else {
            std::process::Command::new("open")
                .arg("-R")
                .arg(entry.path())
                .spawn()
                .map(|_| ())
        };
        if let Err(err) = result {
            self.notice = format!("无法打开：{err}");
        }
    }

    fn copy_path(&mut self) {
        let Some(entry) = self.selected.and_then(|i| self.rows.get(i)) else {
            return;
        };
        match copy_to_clipboard(&entry.path()) {
            Ok(()) => self.status = "已复制路径。".into(),
            Err(err) => self.notice = format!("复制失败：{err}"),
        }
    }

    fn ui_top(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("搜索");
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("中文 / 拼音 / 空格 AND / !排除 / ext:pdf / size:>1mb")
                    .desired_width(f32::INFINITY),
            );
            if !self.focused_once {
                // Put the caret in the search box on launch so typing works
                // immediately after the window appears.
                response.request_focus();
                self.focused_once = true;
            }
            if response.changed() {
                self.run_search();
            }
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.run_search();
                self.dirty = false;
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("kind")
                .selected_text(KINDS[self.kind].0)
                .show_ui(ui, |ui| {
                    for (position, (label, _)) in KINDS.iter().enumerate() {
                        if ui
                            .selectable_value(&mut self.kind, position, *label)
                            .clicked()
                        {
                            self.run_search();
                        }
                    }
                });
            if ui.button("添加目录…").clicked() {
                self.add_root();
            }
            let start = ui.add_enabled(
                !self.scanning && !self.roots.is_empty(),
                egui::Button::new("开始索引"),
            );
            if start.clicked() {
                self.start_scan();
            }
            if ui
                .add_enabled(self.scanning, egui::Button::new("停止"))
                .clicked()
            {
                self.stop_scan();
                self.status = "已停止索引。".into();
            }
            ui.separator();
            let enabled = self.selected.is_some();
            if ui.add_enabled(enabled, egui::Button::new("打开")).clicked() {
                self.action(true);
            }
            if ui
                .add_enabled(enabled, egui::Button::new("在 Finder 显示"))
                .clicked()
            {
                self.action(false);
            }
            if ui
                .add_enabled(enabled, egui::Button::new("复制路径"))
                .clicked()
            {
                self.copy_path();
            }
        });
    }

    fn ui_roots(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(format!("索引范围（{}）", self.roots.len()))
            .default_open(self.roots.is_empty())
            .show(ui, |ui| {
                if self.roots.is_empty() {
                    ui.label("尚未添加目录。默认不扫描任何位置。");
                }
                let mut remove = None;
                for (position, root) in self.roots.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.monospace(root.display().to_string());
                        if ui.small_button("移除").clicked() {
                            remove = Some(position);
                        }
                    });
                }
                if let Some(position) = remove {
                    self.remove_root(position);
                }
            });
    }

    fn ui_results(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("文件名 / 路径");
        });
        ui.separator();
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_height, self.rows.len(), |ui, range| {
                for position in range {
                    let entry = &self.rows[position];
                    let selected = self.selected == Some(position);
                    let label = format!("{}\t{}", entry.name(), entry.path().display());
                    let response = ui.selectable_label(selected, label);
                    if response.clicked() {
                        self.selected = Some(position);
                    }
                    if response.double_clicked() {
                        self.selected = Some(position);
                        self.action(true);
                    }
                }
            });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scan();

        if self.scanning {
            ctx.request_repaint_after(Duration::from_millis(120));
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            self.ui_top(ui);
            ui.add_space(4.0);
            self.ui_roots(ui);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(2.0);
            let progress = if self.scanning {
                format!(
                    " · 扫描中 {} 项，跳过 {}",
                    self.progress.visited, self.progress.skipped
                )
            } else {
                String::new()
            };
            let hits = if self.total > 0 {
                format!(" · 命中 {} 项，显示 {}", self.total, self.rows.len())
            } else {
                String::new()
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{}{progress}{hits}", self.status));
                if !self.notice.is_empty() {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(180, 95, 6), &self.notice);
                }
            });
            ui.add_space(2.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_results(ui);
        });
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Config {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    roots: Vec<PathBuf>,
    #[serde(default)]
    cache_roots: Vec<PathBuf>,
}

/// egui ships Latin-only fonts; without this every Chinese label renders as tofu.
fn install_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: [&str; 4] = [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
    ];
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("cjk".to_owned(), egui::FontData::from_owned(bytes));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, "cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        return;
    }
}

fn storage_paths() -> (Option<PathBuf>, Option<PathBuf>) {
    let base = std::env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join("Library")
            .join("Application Support")
            .join("OilFind")
    });
    match base {
        Some(base) => (
            Some(base.join("config.json")),
            Some(base.join("index.json")),
        ),
        None => (None, None),
    }
}

// Validate the original components before matching the scanner's lexical cleaning.
fn clean_root(path: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let text = path.to_str()?;
    if !path.is_absolute() || text.starts_with("//") || text.starts_with(r"\\") {
        return None;
    }
    safe_config::validate_directory(path).ok()?;
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => cleaned.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::Normal(part) => cleaned.push(part),
        }
    }
    Some(cleaned)
}

// The core deduplicates case-insensitively, including on macOS.
fn path_under(path: &Path, root: &Path) -> bool {
    let path = path.to_string_lossy().to_lowercase();
    let root = root.to_string_lossy().to_lowercase();
    #[cfg(windows)]
    let (path, root) = (path.replace('/', "\\"), root.replace('/', "\\"));
    let separator = std::path::MAIN_SEPARATOR;
    let root = root.trim_end_matches(separator);
    path.trim_end_matches(separator) == root || path.starts_with(&format!("{root}{separator}"))
}

fn insert_root(roots: &mut Vec<PathBuf>, root: PathBuf) -> bool {
    if roots.iter().any(|existing| path_under(&root, existing)) {
        return false;
    }
    roots.retain(|existing| !path_under(existing, &root));
    roots.push(root);
    true
}

fn same_roots(left: &[PathBuf], right: &[PathBuf]) -> bool {
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort();
    right.sort();
    left == right
}

fn copy_to_clipboard(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(path.to_string_lossy().as_bytes())?;
    }
    let _ = child.wait();
    Ok(())
}
