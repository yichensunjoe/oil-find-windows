//! The indexed unit of data: file system metadata plus search metadata.

use pinyin::ToPinyin;
use serde::{
    de::{self, Deserializer},
    Deserialize, Serialize, Serializer,
};
use std::path::{Path, PathBuf};

/// Longest file name accepted from a cache document.
pub(crate) const MAX_NAME_LEN: usize = 1_024;

/// One indexed file or directory.
///
/// Only metadata is stored: file contents are never opened. `name` and the
/// lowercase forms are **derived** from `path` instead of being stored, and the
/// pinyin forms only exist for names that actually contain Han text. A 1.5M
/// entry index saves roughly 130 bytes per entry this way.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    pub modified: i64,
    pub is_dir: bool,
    pub(crate) lower_path: Box<str>,
    pub(crate) pinyin: Option<Box<Pinyin>>,
}

/// Pinyin forms of a name that contains Han text.
#[derive(Debug, Clone)]
pub(crate) struct Pinyin {
    pub(crate) full: Box<str>,
    pub(crate) initials: Box<str>,
}

/// Wire form of an entry. The persisted shape keeps `name` so caches written by
/// the earlier implementation stay readable. Reading and writing use separate
/// shapes because the reader cannot borrow from a stream.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EntryWire<'a> {
    path: &'a str,
    name: &'a str,
    size: u64,
    modified: i64,
    is_dir: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntryOwned {
    path: String,
    // Required: a document without a name is treated as corrupt, exactly like
    // the previous implementation did.
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    modified: i64,
    #[serde(default)]
    is_dir: bool,
}

impl Serialize for Entry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let text = self.path.to_string_lossy();
        EntryWire {
            path: &text,
            name: self.name(),
            size: self.size,
            modified: self.modified,
            is_dir: self.is_dir,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Entry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = EntryOwned::deserialize(deserializer)?;
        if wire.name.len() > MAX_NAME_LEN {
            return Err(de::Error::custom("index entry name is too long"));
        }
        let mut entry = Entry {
            path: PathBuf::from(wire.path),
            size: wire.size,
            modified: wire.modified,
            is_dir: wire.is_dir,
            lower_path: String::new().into_boxed_str(),
            pinyin: None,
        };
        entry.prepare();
        Ok(entry)
    }
}

impl Entry {
    /// Builds an entry and immediately prepares its search metadata.
    ///
    /// `name` is accepted for source compatibility and ignored: the stored name
    /// always comes from `path`, which is the single source of truth.
    pub fn new(path: PathBuf, name: String, size: u64, modified: i64, is_dir: bool) -> Self {
        let mut entry = Entry {
            path,
            size,
            modified,
            is_dir,
            lower_path: String::new().into_boxed_str(),
            pinyin: None,
        };
        let _ = name;
        entry.prepare();
        entry
    }

    /// File name, sliced out of `path` without another allocation.
    pub fn name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    }

    /// Lowercased name, used for case-insensitive matching and ranking.
    ///
    /// `lower_path` is the lowercased full path, so its last segment is exactly
    /// the lowercased name: one slice, no allocation, no second string.
    pub fn lower_name(&self) -> &str {
        let path = &*self.lower_path;
        match path.rfind(['/', '\\']) {
            Some(index) => &path[index + 1..],
            None => path,
        }
    }

    /// Lowercased full path, used for matching and deterministic ordering.
    pub fn lower_path(&self) -> &str {
        &self.lower_path
    }

    /// Lowercased full pinyin of the name (empty when the name has no Han text).
    pub fn full_pinyin(&self) -> &str {
        self.pinyin.as_ref().map(|p| &*p.full).unwrap_or_default()
    }

    /// Lowercased pinyin initials of the name (empty when there is no Han text).
    pub fn initials(&self) -> &str {
        self.pinyin
            .as_ref()
            .map(|p| &*p.initials)
            .unwrap_or_default()
    }

    /// Rebuilds the derived search metadata. Must run after deserialization.
    pub(crate) fn prepare(&mut self) {
        self.lower_path = self.path.to_string_lossy().to_lowercase().into_boxed_str();
        self.pinyin = None;
        if !self.name().chars().any(is_han) {
            return;
        }
        // `to_pinyin` yields one item per character; Han characters contribute
        // their pinyin and everything else contributes itself lowercased, so
        // that "报告2024" is found by both "baogao2024" and "bg2024".
        let mut full = String::with_capacity(self.name().len());
        let mut initials = String::with_capacity(self.name().len());
        for ch in self.name().chars() {
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
        self.pinyin = Some(Box::new(Pinyin {
            full: full.into_boxed_str(),
            initials: initials.into_boxed_str(),
        }));
    }
}

impl PartialEq for Entry {
    /// Compares only the persisted metadata, never the derived search fields.
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
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
