//! The indexed unit of data: file system metadata plus search metadata.

use pinyin::ToPinyin;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One indexed file or directory.
///
/// Only metadata is stored: file contents are never opened. The four
/// pre-computed search fields are derived from `name`/`path` and are not part
/// of the serialized form, so they are rebuilt whenever an index is loaded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub modified: i64,
    #[serde(default)]
    pub is_dir: bool,
    #[serde(skip)]
    pub(crate) lower_name: String,
    #[serde(skip)]
    pub(crate) lower_path: String,
    #[serde(skip)]
    pub(crate) full_pinyin: String,
    #[serde(skip)]
    pub(crate) initials: String,
}

impl Entry {
    /// Builds an entry and immediately prepares its search metadata.
    pub fn new(path: PathBuf, name: String, size: u64, modified: i64, is_dir: bool) -> Self {
        let mut entry = Entry {
            path,
            name,
            size,
            modified,
            is_dir,
            lower_name: String::new(),
            lower_path: String::new(),
            full_pinyin: String::new(),
            initials: String::new(),
        };
        entry.prepare();
        entry
    }

    /// Lowercased name, used for case-insensitive matching and ranking.
    pub fn lower_name(&self) -> &str {
        &self.lower_name
    }

    /// Lowercased full path, used for deterministic ordering.
    pub fn lower_path(&self) -> &str {
        &self.lower_path
    }

    /// Lowercased full pinyin of the name (empty when the name has no Han text).
    pub fn full_pinyin(&self) -> &str {
        &self.full_pinyin
    }

    /// Lowercased pinyin initials of the name (empty when the name has no Han text).
    pub fn initials(&self) -> &str {
        &self.initials
    }

    /// Rebuilds the derived search metadata. Must run after deserialization.
    pub(crate) fn prepare(&mut self) {
        self.lower_name = self.name.to_lowercase();
        self.lower_path = self.path.to_string_lossy().to_lowercase();
        self.full_pinyin.clear();
        self.initials.clear();
        if !self.name.chars().any(is_han) {
            return;
        }
        // `to_pinyin` yields one item per character; Han characters contribute
        // their pinyin and everything else contributes itself lowercased, so
        // that "报告2024" is found by both "baogao2024" and "bg2024".
        let mut full = String::with_capacity(self.name.len());
        let mut initials = String::with_capacity(self.name.len());
        for ch in self.name.chars() {
            match ch.to_pinyin() {
                Some(pinyin) => {
                    let plain = pinyin.plain();
                    full.push_str(plain);
                    if let Some(first) = plain.chars().next() {
                        initials.push(first);
                    }
                }
                None => {
                    for lower in ch.to_lowercase() {
                        full.push(lower);
                        initials.push(lower);
                    }
                }
            }
        }
        self.full_pinyin = full;
        self.initials = initials;
    }
}

impl PartialEq for Entry {
    /// Compares only the persisted metadata, never the derived search fields.
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.name == other.name
            && self.size == other.size
            && self.modified == other.modified
            && self.is_dir == other.is_dir
    }
}

impl Eq for Entry {}

/// Han characters that are worth transliterating.
fn is_han(ch: char) -> bool {
    (0x3400..=0x9fff).contains(&(ch as u32))
}

/// Progress of a scan. `visited` counts indexed entries, `skipped` counts
/// entries or directories that were rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub visited: usize,
    pub skipped: usize,
    pub current: PathBuf,
}

/// A completed scan.
///
/// `Index` serializes to the cache document (see the `cache` module) and
/// deserializes from it, rebuilding the derived search fields of every entry.
#[derive(Debug, Clone, Default)]
pub struct Index {
    pub entries: Vec<Entry>,
    pub roots: Vec<PathBuf>,
    pub built_at: i64,
}

impl Index {
    /// Returns the entry count, mostly useful for logging and tests.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing was indexed.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Lowercased extension including the dot, matching Go's `filepath.Ext` for the
/// inputs this crate cares about (`""` when the name has no dot).
pub(crate) fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(index) => name[index..].to_lowercase(),
        None => String::new(),
    }
}

/// File name of a path without touching the file system.
pub(crate) fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
