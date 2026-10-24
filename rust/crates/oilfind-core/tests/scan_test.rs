//! Scanning: metadata, skip rules, link safety, cancellation and root limits.

mod common;

use common::{now_secs, TempDir};
use oilfind_core::{scan, Entry, Error, Index, Progress, MAX_ROOTS};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Collects progress reports without requiring the callback to be `FnMut`.
#[derive(Default)]
struct Reports {
    seen: Mutex<Vec<Progress>>,
}

impl Reports {
    fn sink(&self) -> impl Fn(Progress) + Sync + '_ {
        |progress| self.seen.lock().expect("progress lock").push(progress)
    }

    fn take(&self) -> Vec<Progress> {
        std::mem::take(&mut *self.seen.lock().expect("progress lock"))
    }
}

fn indexed_paths(index: &Index) -> HashMap<PathBuf, Entry> {
    let mut paths = HashMap::new();
    for entry in &index.entries {
        let previous = paths.insert(entry.path.clone(), entry.clone());
        assert!(
            previous.is_none(),
            "duplicate indexed path: {:?}",
            entry.path
        );
    }
    paths
}

fn run_scan(roots: &[PathBuf]) -> Result<Index, Error> {
    let cancel = AtomicBool::new(false);
    scan(&cancel, roots, |_| {})
}

#[test]
fn scan_reports_metadata_and_deduplicates_overlapping_roots() {
    let dir = TempDir::new("scan-basic");
    let child = dir.mkdir("documents");
    let file = dir.write("documents/会议纪要.PDF", "fixture metadata only");
    dir.write("a.txt", "sample");

    let cancel = AtomicBool::new(false);
    let reports = Reports::default();
    let index = scan(
        &cancel,
        &[
            dir.path().to_path_buf(),
            child.clone(),
            dir.path().to_path_buf(),
            child.join("."),
        ],
        reports.sink(),
    )
    .expect("scan");

    // The child root is covered by its parent, so it is walked exactly once.
    assert_eq!(index.roots.len(), 1);
    let paths = indexed_paths(&index);
    let entry = paths.get(&file).expect("selected file was not indexed");
    let metadata = fs::metadata(&file).expect("fixture metadata");
    assert_eq!(entry.name, "会议纪要.PDF");
    assert!(!entry.is_dir);
    assert_eq!(entry.size, metadata.len());
    assert_eq!(entry.modified, modified_secs(&metadata));

    let directory = paths.get(&child).expect("directory metadata missing");
    assert!(directory.is_dir);
    assert_eq!(directory.size, 0);
    assert!(
        !paths.contains_key(dir.path()),
        "the root itself is not an entry"
    );

    // Entries are sorted by lowercased path so the cache is deterministic.
    let mut sorted = index.entries.clone();
    sorted.sort_by(|a, b| a.lower_path().cmp(b.lower_path()));
    assert_eq!(
        index
            .entries
            .iter()
            .map(Entry::lower_path)
            .collect::<Vec<_>>(),
        sorted.iter().map(Entry::lower_path).collect::<Vec<_>>()
    );

    let reports = reports.take();
    assert!(!reports.is_empty(), "scan did not report progress");
    let mut previous = 0;
    for report in &reports {
        assert!(report.visited >= previous, "visited went backwards");
        previous = report.visited;
    }
}

fn modified_secs(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("mtime after epoch")
        .as_secs() as i64
}

#[test]
fn scan_skips_dependency_system_and_private_directories() {
    let dir = TempDir::new("scan-skips");
    // Project-internal and private runtime folders are skipped at any depth.
    let blocked = [
        "node_modules",
        ".git",
        ".claude",
        ".codex",
        ".gemini",
        ".qwen",
        ".windsurf",
        ".workbuddy",
        ".workbuddy-ai",
        ".freebuff",
        ".codegraph",
        ".pnpm-store",
        ".config",
        ".agents",
        ".build",
        ".svn",
        ".hg",
    ];
    for name in blocked {
        dir.write(&format!("{name}/must-not-index.txt"), "isolated fixture");
    }
    let visible = dir.write("documents/visible.txt", "visible");
    // Volume-root-only system names must stay searchable when nested inside a
    // user-selected project (D:\code\myproject\windows, D:\work\tmp).
    let nested = [
        "windows",
        "tmp",
        ".tmp",
        "Program Files",
        "Program Files (x86)",
        "ProgramData",
        "System Volume Information",
        "$RECYCLE.BIN",
        "Recovery",
        "Config.Msi",
    ];
    for name in nested {
        dir.write(&format!("project/{name}/nested.txt"), "ordinary fixture");
    }

    let index = run_scan(&[dir.path().to_path_buf()]).expect("scan");
    let paths = indexed_paths(&index);
    assert!(paths.contains_key(&visible), "ordinary file was skipped");
    for name in blocked {
        assert!(
            !paths.contains_key(&dir.join(&format!("{name}/must-not-index.txt"))),
            "scanned excluded directory {name:?}"
        );
    }
    for name in nested {
        assert!(
            paths.contains_key(&dir.join(&format!("project/{name}/nested.txt"))),
            "nested ordinary directory {name:?} was hidden"
        );
    }
}

#[cfg(unix)]
#[test]
fn scan_never_follows_symlinks() {
    let dir = TempDir::new("scan-links");
    let root = dir.mkdir("selected");
    let outside = dir.mkdir("outside");
    let outside_file = common::write_file(
        &outside.join("outside-secret.txt"),
        "temporary fixture only",
    );
    common::write_file(
        &outside.join("nested/nested-secret.txt"),
        "temporary fixture only",
    );
    common::write_file(&root.join("safe.txt"), "safe");

    let linked_dir = root.join("linked-directory");
    let linked_file = root.join("linked-file.txt");
    std::os::unix::fs::symlink(&outside, &linked_dir).expect("symlink directory");
    std::os::unix::fs::symlink(&outside_file, &linked_file).expect("symlink file");

    let index = run_scan(std::slice::from_ref(&root)).expect("scan");
    let paths = indexed_paths(&index);
    for forbidden in [
        outside_file.clone(),
        linked_dir.clone(),
        linked_file.clone(),
        linked_dir.join("outside-secret.txt"),
    ] {
        assert!(
            !paths.contains_key(&forbidden),
            "symlink path or target was indexed: {forbidden:?}"
        );
    }
    assert!(paths.contains_key(&root.join("safe.txt")));

    // An explicit link root, or a root under a link, is refused outright. A
    // leaf link is rejected as "not a directory" (Lstat never follows it); a
    // path that only *passes through* a link is rejected by the ancestor walk.
    for selected in [linked_dir.clone(), linked_dir.join("nested")] {
        let error = run_scan(&[selected]).expect_err("link root must be rejected");
        assert!(
            matches!(error, Error::LinkRoot | Error::NotDirectory),
            "unexpected error: {error}"
        );
    }
    let error = run_scan(&[linked_dir.join("nested")]).expect_err("ancestor link");
    assert!(
        matches!(error, Error::LinkRoot),
        "unexpected error: {error}"
    );
}

#[test]
fn scan_rejects_protected_ancestors_and_non_directory_roots() {
    let dir = TempDir::new("scan-protected");
    let protected = dir.write(".codex/nested/fixture.txt", "temporary only");
    let protected_dir = protected.parent().expect("parent").to_path_buf();
    let plain_file = dir.write("plain.txt", "file root must be rejected");

    for selected in [
        protected_dir.parent().expect("parent").to_path_buf(),
        protected_dir,
        plain_file,
        PathBuf::from("relative-fixture"),
    ] {
        assert!(
            run_scan(std::slice::from_ref(&selected)).is_err(),
            "accepted protected or non-directory root: {selected:?}"
        );
    }

    let too_many: Vec<PathBuf> = (0..MAX_ROOTS + 1)
        .map(|_| dir.path().to_path_buf())
        .collect();
    assert!(run_scan(&too_many).is_err(), "accepted more than 64 roots");
    let exactly: Vec<PathBuf> = (0..MAX_ROOTS).map(|_| dir.path().to_path_buf()).collect();
    assert!(
        run_scan(&exactly).is_ok(),
        "64 identical roots should collapse into one"
    );
}

#[test]
fn scan_rejects_missing_selection_and_network_roots() {
    let cases: Vec<Vec<PathBuf>> = vec![
        Vec::new(),
        vec![PathBuf::from("")],
        vec![PathBuf::from("   ")],
        vec![PathBuf::from(r"\\server\share")],
        vec![PathBuf::from("//server/share")],
        vec![PathBuf::from(r"\\?\UNC\server\share")],
        vec![PathBuf::from(r"\\?\C:\Windows")],
        vec![PathBuf::from("/definitely/not/here")],
    ];
    for roots in cases {
        assert!(
            run_scan(&roots).is_err(),
            "accepted invalid or network roots: {roots:?}"
        );
    }
}

#[test]
fn scan_cancellation_never_returns_a_partial_index() {
    let dir = TempDir::new("scan-cancel");
    for index in 0..100 {
        dir.write(&format!("sample-{index:03}.txt"), "temporary");
    }

    let cancel = AtomicBool::new(true);
    let error =
        scan(&cancel, &[dir.path().to_path_buf()], |_| {}).expect_err("a cancelled scan must fail");
    assert!(matches!(error, Error::Cancelled));

    let cancel = AtomicBool::new(false);
    let error = scan(&cancel, &[dir.path().to_path_buf()], |progress| {
        if progress.visited >= 1 {
            cancel.store(true, Ordering::Relaxed);
        }
    })
    .expect_err("cancelling from the progress callback must fail the scan");
    assert!(matches!(error, Error::Cancelled));
}

#[cfg(unix)]
#[test]
fn scan_skips_unreadable_directories_and_keeps_going() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("scan-unreadable");
    let denied = dir.write("denied/private.txt", "fixture");
    let denied = denied.parent().expect("parent").to_path_buf();
    let visible = dir.write("visible.txt", "visible");

    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).expect("chmod");
    if fs::read_dir(&denied).is_ok() {
        // Running as root: permissions cannot hide the directory.
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).expect("chmod");
        return;
    }

    let cancel = AtomicBool::new(false);
    let reports = Reports::default();
    let index = scan(&cancel, &[dir.path().to_path_buf()], reports.sink()).expect("scan");
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).expect("chmod");

    let paths = indexed_paths(&index);
    assert!(!paths.contains_key(&denied.join("private.txt")));
    assert!(paths.contains_key(&visible));
    let skipped = reports
        .take()
        .iter()
        .map(|report| report.skipped)
        .max()
        .unwrap_or(0);
    assert!(skipped >= 1, "unreadable directory was not reported");
}

#[test]
fn scan_accepts_a_directory_created_just_now() {
    let dir = TempDir::new("scan-empty");
    let index = run_scan(&[dir.path().to_path_buf()]).expect("scan");
    assert!(index.entries.is_empty());
    assert_eq!(index.roots.len(), 1);
    assert!(index.built_at <= now_secs());
    assert!(index.is_empty());
    assert_eq!(index.len(), 0);
}

#[cfg(unix)]
#[test]
fn scan_ignores_non_regular_files() {
    let dir = TempDir::new("scan-special");
    let socket = dir.path().join("pipe.sock");
    let listener = match std::os::unix::net::UnixListener::bind(&socket) {
        Ok(listener) => listener,
        // Socket paths are length limited; a long temporary directory can
        // legitimately refuse the fixture.
        Err(_) => return,
    };
    dir.write("regular.txt", "regular");
    let index = run_scan(&[dir.path().to_path_buf()]).expect("scan");
    let paths = indexed_paths(&index);
    assert!(paths.contains_key(&dir.join("regular.txt")));
    assert!(!paths.contains_key(&socket), "socket was indexed");
    drop(listener);
}

#[test]
fn scan_root_must_be_a_directory() {
    let dir = TempDir::new("scan-root-file");
    let file = dir.write("only.txt", "not a directory");
    let error = run_scan(std::slice::from_ref(&file)).expect_err("file root");
    assert!(matches!(error, Error::NotDirectory), "got: {error}");
}

#[test]
fn scan_rejects_a_root_inside_a_skipped_directory() {
    let dir = TempDir::new("scan-inside-skipped");
    let nested = dir.mkdir("node_modules/pkg");
    let error = run_scan(&[nested]).expect_err("skipped ancestor");
    assert!(matches!(error, Error::ProtectedRoot), "got: {error}");
}

#[test]
fn scanned_entries_are_searchable_immediately() {
    let dir = TempDir::new("scan-search");
    dir.write("docs/季度报告.docx", "fixture");
    let index = run_scan(&[dir.path().to_path_buf()]).expect("scan");
    let (results, total) = index.search("jidubaogao", None, 10).expect("search");
    assert_eq!(total, 1);
    assert_eq!(results[0].name, "季度报告.docx");
    let (results, _) = index.search("jd", None, 10).expect("search");
    assert_eq!(results[0].name, "季度报告.docx");
}

#[test]
fn scan_is_deterministic_across_runs() {
    let dir = TempDir::new("scan-determinism");
    for index in 0..20 {
        dir.write(&format!("dir-{index}/file-{index}.txt"), "fixture");
    }
    let first = run_scan(&[dir.path().to_path_buf()]).expect("scan");
    let second = run_scan(&[dir.path().to_path_buf()]).expect("scan");
    let paths =
        |index: &Index| -> Vec<PathBuf> { index.entries.iter().map(|e| e.path.clone()).collect() };
    assert_eq!(paths(&first), paths(&second));
}
