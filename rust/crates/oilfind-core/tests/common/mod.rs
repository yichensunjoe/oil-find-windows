//! Shared fixtures for the integration tests.
//!
//! Every fixture lives under the system temporary directory and is removed when
//! the guard is dropped. No test ever touches a real user directory.

#![allow(dead_code)]

use oilfind_core::{Entry, Index};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary directory that cleans itself up.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> TempDir {
        // Prefer Cargo's per-target temp dir: on Windows the system temp dir
        // lives under AppData, which the scanner rejects by policy.
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        fs::create_dir_all(&base).expect("create target temp dir");
        for _ in 0..128 {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.subsec_nanos())
                .unwrap_or(0);
            let candidate = base.join(format!(
                "oilfind-test-{}-{label}-{nanos}-{unique}",
                std::process::id()
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    // The temporary directory itself may be reached through a
                    // symlink (/var -> /private/var on macOS); scanning rejects
                    // linked ancestors, so resolve it first.
                    let path = fs::canonicalize(&candidate).expect("canonicalize fixture");
                    return TempDir { path };
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("cannot create fixture directory: {err}"),
            }
        }
        panic!("cannot allocate a unique fixture directory");
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, relative: &str) -> PathBuf {
        self.path.join(relative)
    }

    /// Writes `data` to `relative`, creating parent directories.
    pub fn write(&self, relative: &str, data: &str) -> PathBuf {
        write_file(&self.join(relative), data)
    }

    /// Creates an empty directory tree.
    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.join(relative);
        fs::create_dir_all(&path).expect("create fixture directory");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // A permission fixture may have left a directory unreadable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o700));
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn write_file(path: &Path, data: &str) -> PathBuf {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, data).expect("write fixture file");
    path.to_path_buf()
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Today's date in the machine's local time zone, which is what `dm:` uses.
///
/// Computing this in UTC would make the test fail between local midnight and
/// the UTC offset (for example 00:00-08:00 in UTC+8).
pub fn today_local() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// The same fixture set the Go implementation's search tests use.
pub fn search_fixture() -> (TempDir, Index) {
    let dir = TempDir::new("search");
    let root = dir.path().to_path_buf();
    let now = now_secs();
    let old = now - 40 * 86_400;
    const MIB: u64 = 1024 * 1024;

    let entries = vec![
        entry(&root, "reports/Report Final.PDF", 12 * MIB, now, false),
        entry(&root, "reports/report draft.docx", 3 * MIB, old, false),
        entry(&root, "notes/report notes.txt", MIB / 2, old, false),
        entry(&root, "images/IMG_001.PNG", 2 * MIB, now, false),
        entry(&root, "images/photo.jpg", MIB / 4, old, false),
        entry(&root, "docs/会议纪要.docx", MIB, now, false),
        entry(&root, "docs/预算报告.pdf", 5 * MIB, old, false),
        entry(&root, "source/main.go", 2048, old, false),
        entry(&root, "reports", 0, now, true),
        entry(&root, "archive/report old.pdf", 20 * MIB, old, false),
    ];
    (
        dir,
        Index {
            entries,
            roots: vec![root],
            built_at: now,
        },
    )
}

/// Builds one entry whose name is the last path segment.
pub fn entry(root: &Path, relative: &str, size: u64, modified: i64, is_dir: bool) -> Entry {
    let path = root.join(relative);
    let name = path
        .file_name()
        .expect("fixture has a file name")
        .to_string_lossy()
        .into_owned();
    Entry::new(path, name, size, modified, is_dir)
}

/// Runs a query and returns the matching names, sorted for comparison.
pub fn names(index: &Index, query: &str) -> Vec<String> {
    let (results, _) = index
        .search(query, None, 100)
        .unwrap_or_else(|err| panic!("search({query:?}) failed: {err}"));
    sorted_names(&results)
}

pub fn filtered_names(index: &Index, query: &str, kind: &str) -> Vec<String> {
    let (results, _) = index
        .search(query, Some(kind), 100)
        .unwrap_or_else(|err| panic!("search({query:?}, {kind:?}) failed: {err}"));
    sorted_names(&results)
}

pub fn sorted_names(entries: &[Entry]) -> Vec<String> {
    let mut names: Vec<String> = entries.iter().map(|entry| entry.name.clone()).collect();
    names.sort();
    names
}

pub fn sorted_expected(expected: &[&str]) -> Vec<String> {
    let mut expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
    expected.sort();
    expected
}

/// Escapes a path for embedding in a hand-written JSON document.
///
/// Windows fixtures contain backslashes, which are invalid JSON escapes unless
/// doubled; the Unix-only tests never hit this, so it must be explicit.
pub fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
