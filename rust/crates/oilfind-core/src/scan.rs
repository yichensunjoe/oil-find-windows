//! Directory scanning: enumerate metadata under explicitly supplied roots.

use crate::entry::{file_name_of, Entry, Index, Progress};
use crate::error::Error;
use crate::platform;
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};
use walkdir::{DirEntry, WalkDir};

/// Hard ceiling on the number of indexed entries.
pub const MAX_ENTRIES: usize = 2_000_000;

/// Hard ceiling on the number of roots a single index may cover.
pub const MAX_ROOTS: usize = 64;

/// How often the cancellation flag is sampled while walking.
const CANCEL_CHECK_INTERVAL: usize = 256;

/// Minimum interval between progress callbacks, so a large scan does not flood
/// the UI thread with events.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);

/// Directories that are internal to a project or to a private agent runtime.
/// Their names are specific enough that skipping them at any depth does not
/// hide ordinary user data.
const EXCLUDED_AT_ANY_DEPTH: &[&str] = &[
    "node_modules",
    ".git",
    ".svn",
    ".hg",
    ".claude",
    ".codex",
    ".workbuddy",
    ".workbuddy-ai",
    ".gemini",
    ".qwen",
    ".pnpm-store",
    ".build",
    ".windsurf",
    ".freebuff",
    ".codegraph",
    ".agents",
    ".config",
];

/// System directories that are only skipped when they sit directly on a volume
/// root. Matching them at any depth would hide ordinary folders such as
/// `D:\code\myproject\windows` or `D:\work\tmp`.
const SYSTEM_ROOT_DIRS: &[&str] = &[
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "$recycle.bin",
    "system volume information",
    "recovery",
    "config.msi",
    "tmp",
    ".tmp",
];

/// Walks `roots` and returns a metadata-only index.
///
/// The scan is all-or-nothing: cancelling or hitting [`MAX_ENTRIES`] returns an
/// error instead of a partial index, so callers can never publish a truncated
/// cache.
pub fn scan(
    cancel: &AtomicBool,
    roots: &[PathBuf],
    progress: impl Fn(Progress) + Sync,
) -> Result<Index, Error> {
    let normalized = normalize_roots(roots)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }

    let reporter = Reporter::new(&progress);
    let skipped = Cell::new(0usize);
    let mut entries: Vec<Entry> = Vec::with_capacity(1024);
    let mut visited = 0usize;
    let mut since_cancel_check = 0usize;

    for root in &normalized {
        let mut walker = WalkDir::new(root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| keep_entry(entry, &skipped));

        while let Some(item) = walker.next() {
            since_cancel_check += 1;
            if since_cancel_check >= CANCEL_CHECK_INTERVAL {
                since_cancel_check = 0;
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
            }
            let entry = match item {
                Ok(entry) => entry,
                Err(err) => {
                    // Unreadable directories and vanished entries are counted
                    // and skipped; one bad branch must not abort the scan.
                    skipped.set(skipped.get() + 1);
                    reporter.emit(
                        false,
                        visited,
                        skipped.get(),
                        err.path().unwrap_or(root.as_path()),
                    );
                    continue;
                }
            };
            let path = entry.path();
            if entry.depth() == 0 {
                continue;
            }
            if entry.file_type().is_dir() && is_package_dir(path) {
                // Index the bundle itself (so Xcode.app is findable) but never
                // walk its internals: a single .app can hold hundreds of
                // thousands of files.
                walker.skip_current_dir();
            }
            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    skipped.set(skipped.get() + 1);
                    reporter.emit(false, visited, skipped.get(), path);
                    continue;
                }
            };
            if platform::is_link_or_reparse(&metadata) {
                skipped.set(skipped.get() + 1);
                reporter.emit(false, visited, skipped.get(), path);
                continue;
            }
            let is_dir = metadata.is_dir();
            if is_dir && should_skip_dir(path) {
                skipped.set(skipped.get() + 1);
                reporter.emit(false, visited, skipped.get(), path);
                continue;
            }
            if !is_dir && !metadata.is_file() {
                skipped.set(skipped.get() + 1);
                continue;
            }
            let size = if is_dir { 0 } else { metadata.len() };
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_secs() as i64)
                .unwrap_or(0);
            entries.push(Entry::new(
                path.to_path_buf(),
                file_name_of(path),
                size,
                modified,
                is_dir,
            ));
            visited += 1;
            if entries.len() >= MAX_ENTRIES {
                return Err(Error::EntryLimit);
            }
            reporter.emit(false, visited, skipped.get(), path);
        }
    }

    // Deterministic order, independent of directory iteration order.
    entries.sort_by(|a, b| a.lower_path().cmp(b.lower_path()));
    let last_root = normalized.last().map_or_else(PathBuf::new, Clone::clone);
    reporter.emit(true, visited, skipped.get(), &last_root);
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok(Index {
        entries,
        roots: normalized,
        built_at: crate::time::now_secs(),
    })
}

/// macOS package directories whose contents are implementation details.
fn is_package_dir(path: &Path) -> bool {
    const SUFFIXES: [&str; 8] = [
        ".app",
        ".bundle",
        ".framework",
        ".plugin",
        ".kext",
        ".appex",
        ".xpc",
        ".xcodeproj",
    ];
    let name = match path.file_name() {
        Some(name) => name.to_string_lossy().to_lowercase(),
        None => return false,
    };
    SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// `filter_entry` predicate: prunes links and skipped directories before the
/// walker descends into them, which is what keeps `node_modules` cheap.
fn keep_entry(entry: &DirEntry, skipped: &Cell<usize>) -> bool {
    let file_type = entry.file_type();
    if file_type.is_symlink() {
        skipped.set(skipped.get() + 1);
        return false;
    }
    if file_type.is_dir() && should_skip_dir(entry.path()) {
        skipped.set(skipped.get() + 1);
        return false;
    }
    true
}

/// Decides from the path alone so the walker and the root validator apply
/// exactly the same rule.
pub(crate) fn should_skip_dir(path: &Path) -> bool {
    let name = match path.file_name() {
        Some(name) => name.to_string_lossy().to_lowercase(),
        None => return false,
    };
    if EXCLUDED_AT_ANY_DEPTH.contains(&name.as_str()) {
        return true;
    }
    if SYSTEM_ROOT_DIRS.contains(&name.as_str()) {
        return path.parent().is_some_and(platform::is_volume_root);
    }
    // macOS keeps caches, mail and app containers under `<home>/Library` and
    // trash under `<home>/.Trash`; a project folder with the same name must
    // stay searchable, so the rule only fires directly under a user profile.
    if HOME_CACHES.contains(&name.as_str()) {
        return under_user_profile(path);
    }
    if name == "appdata" {
        // Only `<volume>\Users\<profile>\AppData` (and the Unix `/Users/...`,
        // `/home/...` equivalents) is a cache folder. A project folder named
        // appdata must stay searchable.
        return under_user_profile(path);
    }
    false
}

/// Tool caches that live directly under the home directory. They hold hundreds
/// of thousands of files and none of them is something a user searches for.
const HOME_CACHES: [&str; 11] = [
    "library", ".trash", ".cargo", ".rustup", ".cache", ".npm", ".gradle", ".m2", ".conda",
    ".pyenv", ".nvm",
];

/// True for `<volume>/Users/<profile>/<dir>` (macOS/Linux) and
/// `<volume>\Users\<profile>\<dir>` (Windows).
fn under_user_profile(path: &Path) -> bool {
    let Some(profile) = path.parent() else {
        return false;
    };
    let Some(users) = profile.parent() else {
        return false;
    };
    let users_name = users
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase());
    matches!(users_name.as_deref(), Some("users") | Some("home"))
        && users.parent().is_some_and(platform::is_volume_root)
}

/// Validates, cleans and de-duplicates the requested roots.
/// Removes the Windows verbatim prefix so `\\?\C:\dir` is treated as `C:\dir`.
///
/// `\\?\UNC\server\share` becomes `\\server\share` on purpose, so the caller
/// still rejects it as a network path.
fn strip_verbatim_prefix(text: &str) -> Option<String> {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    text.strip_prefix(r"\\?\").map(str::to_string)
}

fn normalize_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>, Error> {
    if roots.is_empty() {
        return Err(Error::EmptyRoots);
    }
    if roots.len() > MAX_ROOTS {
        return Err(Error::TooManyRoots);
    }

    let mut normalized: Vec<PathBuf> = Vec::with_capacity(roots.len());
    for root in roots {
        let original = root.to_string_lossy().into_owned();
        if original.trim().is_empty() {
            return Err(Error::EmptyRoot);
        }
        // `fs::canonicalize` returns a verbatim path on Windows ("\\?\C:\..."),
        // which is still a local drive; unwrap it before the lexical checks.
        let text = strip_verbatim_prefix(&original).unwrap_or(original);
        if text.trim().is_empty() {
            return Err(Error::EmptyRoot);
        }
        // Checked before `is_absolute` because "//server/share" looks absolute
        // on Unix while "\\server\share" never is.
        if text.starts_with(r"\\") || text.starts_with("//") {
            return Err(Error::NetworkRoot);
        }
        let root = PathBuf::from(&text);
        if !root.is_absolute() {
            return Err(Error::RelativeRoot);
        }
        let cleaned = platform::clean_path(&root);
        platform::validate_local_volume(&cleaned)?;
        let metadata = fs::symlink_metadata(&cleaned).map_err(Error::RootUnreadable)?;
        if !metadata.is_dir() {
            return Err(Error::NotDirectory);
        }
        // Every ancestor is checked, not just the leaf the user picked, so a
        // protected or linked parent cannot be used as a back door.
        let mut current: &Path = &cleaned;
        loop {
            let metadata = fs::symlink_metadata(current).map_err(Error::RootUnreadable)?;
            if platform::is_link_or_reparse(&metadata) {
                return Err(Error::LinkRoot);
            }
            if should_skip_dir(current) {
                return Err(Error::ProtectedRoot);
            }
            match current.parent() {
                Some(parent) if parent != current => current = parent,
                _ => break,
            }
        }
        normalized.push(cleaned);
    }

    // Shortest first, so a parent is always seen before its children.
    normalized.sort_by_key(|root| root.as_os_str().len());
    let mut unique: Vec<PathBuf> = Vec::with_capacity(normalized.len());
    for root in normalized {
        if unique.iter().any(|kept| platform::path_under(&root, kept)) {
            continue;
        }
        unique.push(root);
    }
    Ok(unique)
}

/// Throttled progress reporting.
struct Reporter<'a, F: Fn(Progress)> {
    inner: &'a F,
    last: Cell<Instant>,
}

impl<'a, F: Fn(Progress)> Reporter<'a, F> {
    fn new(inner: &'a F) -> Self {
        Reporter {
            inner,
            last: Cell::new(Instant::now()),
        }
    }

    fn emit(&self, force: bool, visited: usize, skipped: usize, current: &Path) {
        if force || self.last.get().elapsed() >= PROGRESS_INTERVAL {
            (self.inner)(Progress {
                visited,
                skipped,
                current: current.to_path_buf(),
            });
            self.last.set(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_depth_exclusions_apply_everywhere() {
        assert!(should_skip_dir(Path::new("/a/node_modules")));
        assert!(should_skip_dir(Path::new("/a/b/.git")));
        assert!(!should_skip_dir(Path::new("/a/b/.github")));
    }

    #[test]
    fn system_directories_only_apply_at_a_volume_root() {
        assert!(should_skip_dir(Path::new("/windows")));
        assert!(should_skip_dir(Path::new("/tmp")));
        assert!(!should_skip_dir(Path::new("/home/joe/project/windows")));
        assert!(!should_skip_dir(Path::new("/home/joe/work/tmp")));
    }

    #[test]
    fn appdata_only_applies_under_a_user_profile() {
        assert!(should_skip_dir(Path::new("/Users/joe/AppData")));
        assert!(should_skip_dir(Path::new("/home/joe/appdata")));
        assert!(should_skip_dir(Path::new("/Users/joe/Library")));
        assert!(should_skip_dir(Path::new("/Users/joe/.cargo")));
        assert!(!should_skip_dir(Path::new("/Users/joe/projects/.cargo")));
        assert!(should_skip_dir(Path::new("/Users/joe/.Trash")));
        assert!(!should_skip_dir(Path::new("/Users/joe/projects/Library")));
        assert!(!should_skip_dir(Path::new("/Volumes/Data/Library")));
        assert!(!should_skip_dir(Path::new("/Users/joe/project/appdata")));
        assert!(!should_skip_dir(Path::new("/Users/joe/AppData/Roaming")));
    }
}
