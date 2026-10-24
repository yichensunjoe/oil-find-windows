//! The indexed unit of data: file system metadata plus search metadata.

use pinyin::ToPinyin;
use serde::{
    de::{self, Deserializer},
    Deserialize, Serialize, Serializer,
};
use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::Arc;

/// Longest file name accepted from a cache document.
pub(crate) const MAX_NAME_LEN: usize = 1_024;

/// One parent directory, shared by every entry that lives in it.
///
/// The original-case and lowercase forms sit in a single allocation, so a
/// directory that holds 40 000 files pays for both strings once, and the
/// ordering comparison reads one cache line instead of chasing two
/// allocations. The two forms are concatenated in `text`, split at `split`.
#[derive(Debug)]
pub(crate) struct SharedDir {
    text: String,
    split: usize,
}

impl SharedDir {
    /// Builds the shared form of one directory path.
    pub(crate) fn new(dir: String) -> Arc<SharedDir> {
        let lower_dir = dir.to_lowercase();
        let mut text = String::with_capacity(dir.len() + lower_dir.len());
        text.push_str(&dir);
        text.push_str(&lower_dir);
        Arc::new(SharedDir {
            text,
            split: dir.len(),
        })
    }

    /// Directory as it appears on disk.
    pub(crate) fn dir(&self) -> &str {
        &self.text[..self.split]
    }

    /// Lowercased directory, used for matching and deterministic ordering.
    pub(crate) fn lower_dir(&self) -> &str {
        &self.text[self.split..]
    }
}

/// One indexed file or directory.
///
/// The parent directory is stored as a shared [`Arc<SharedDir>`], so a
/// directory that holds 40 000 files keeps its path (and its lowercase form)
/// once instead of 40 000 times. `name` is the file name and `pinyin` only
/// exists for names that contain Han text. `lower_name` holds the lowercase
/// form only when it differs from `name`: a name that is ASCII without
/// `A`-`Z` is already its own lowercase form, so the copy would buy nothing.
#[derive(Debug, Clone)]
pub struct Entry {
    pub(crate) shared: Arc<SharedDir>,
    pub name: Box<str>,
    pub size: u64,
    pub modified: i64,
    pub is_dir: bool,
    pub(crate) lower_name: Option<Box<str>>,
    pub(crate) pinyin: Option<Box<Pinyin>>,
}

/// Pinyin forms of a name that contains Han text.
#[derive(Debug, Clone)]
pub(crate) struct Pinyin {
    pub(crate) full: Box<str>,
    pub(crate) initials: Box<str>,
}

/// Wire form of an entry: the persisted shape keeps `name`, and the JSON format
/// keeps the full `path` so caches written by the Go implementation stay
/// readable. Reading and writing use separate shapes because the reader cannot
/// borrow from a stream.
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

impl Entry {
    /// Builds an entry and immediately prepares its search metadata. `name` is
    /// accepted for source compatibility; the stored name comes from `path`.
    pub fn new(path: PathBuf, name: String, size: u64, modified: i64, is_dir: bool) -> Self {
        let dir = path
            .parent()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or(name);
        let mut entry = Entry {
            shared: SharedDir::new(dir),
            name: file_name.into_boxed_str(),
            size,
            modified,
            is_dir,
            lower_name: None,
            pinyin: None,
        };
        entry.prepare();
        entry
    }

    /// Full path of the entry. Allocates; call it only for rows a user sees.
    pub fn path(&self) -> PathBuf {
        if self.shared.dir().is_empty() {
            return PathBuf::from(&*self.name);
        }
        PathBuf::from(format!(
            "{dir}{sep}{name}",
            dir = self.shared.dir(),
            sep = std::path::MAIN_SEPARATOR,
            name = self.name
        ))
    }

    /// Builds an entry reusing an already shared directory, which is what makes
    /// siblings cheap: the scanner interns each directory once. The name is
    /// passed on its own because the cache loader has it without a path.
    pub(crate) fn with_dir(
        shared: Arc<SharedDir>,
        name: &str,
        size: u64,
        modified: i64,
        is_dir: bool,
    ) -> Self {
        let mut entry = Entry {
            shared,
            name: Box::from(name),
            size,
            modified,
            is_dir,
            lower_name: None,
            pinyin: None,
        };
        entry.prepare();
        entry
    }

    /// File name of the entry.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Directory of the entry, shared between its siblings.
    pub fn dir(&self) -> &str {
        self.shared.dir()
    }

    /// Lowercased name, used for case-insensitive matching and ranking.
    pub fn lower_name(&self) -> &str {
        match &self.lower_name {
            Some(lower) => lower,
            // No copy was stored, which only happens when the name is ASCII
            // without uppercase letters: it is already its own lowercase form.
            None => &self.name,
        }
    }

    /// Lowercased full path, used for matching and deterministic ordering.
    pub fn lower_path(&self) -> String {
        if self.shared.lower_dir().is_empty() {
            return self.lower_name().to_string();
        }
        format!(
            "{dir}{sep}{name}",
            dir = self.shared.lower_dir(),
            sep = std::path::MAIN_SEPARATOR,
            name = self.lower_name()
        )
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

    pub(crate) fn prepare(&mut self) {
        // Most names are already their own lowercase form: ASCII without
        // `A`-`Z`, and Han text, which has no case at all. Those store no
        // second copy; everything else pays for exactly one.
        self.lower_name = if lowercases_to_itself(&self.name) {
            None
        } else {
            Some(self.name.to_lowercase().into_boxed_str())
        };
        self.pinyin = None;
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
        self.pinyin = Some(Box::new(Pinyin {
            full: full.into_boxed_str(),
            initials: initials.into_boxed_str(),
        }));
    }
}

impl Serialize for Entry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Rebuilding the path here is fine: the JSON form is only written by
        // tests and by tools that inspect a cache, never on the hot path.
        let text = self.path().to_string_lossy().into_owned();
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
        Ok(Entry::new(
            PathBuf::from(wire.path),
            wire.name,
            wire.size,
            wire.modified,
            wire.is_dir,
        ))
    }
}

impl PartialEq for Entry {
    /// Compares only the persisted metadata, never the derived search fields.
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.size == other.size
            && self.modified == other.modified
            && self.is_dir == other.is_dir
    }
}

impl Eq for Entry {}

/// A completed scan.
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

/// Orders two entries the way their full lowercase paths order, without
/// building those paths: the directory is compared first, and when one
/// directory is a prefix of the other the separator decides (`/Users/a-b` sorts
/// before `/Users/a/x` because `-` is smaller than `/`).
pub fn compare_entries(a: &Entry, b: &Entry) -> Ordering {
    let (left_dir, right_dir) = (a.shared.lower_dir(), b.shared.lower_dir());
    let common = left_dir.len().min(right_dir.len());
    match left_dir.as_bytes()[..common].cmp(&right_dir.as_bytes()[..common]) {
        Ordering::Equal if left_dir.len() == right_dir.len() => a.lower_name().cmp(b.lower_name()),
        Ordering::Equal => {
            let (short_dir, short_name, long_dir, long_name) = if left_dir.len() < right_dir.len() {
                (left_dir, a.lower_name(), right_dir, b.lower_name())
            } else {
                (right_dir, b.lower_name(), left_dir, a.lower_name())
            };
            match b'/'.cmp(&long_dir.as_bytes()[common]) {
                // Rare: the longer directory also has a separator here.
                Ordering::Equal => {
                    let short = format!("{short_dir}/{short_name}");
                    let long = format!("{long_dir}/{long_name}");
                    if short_dir.len() == left_dir.len() {
                        short.cmp(&long)
                    } else {
                        long.cmp(&short)
                    }
                }
                order => {
                    if short_dir.len() == left_dir.len() {
                        order
                    } else {
                        order.reverse()
                    }
                }
            }
        }
        order => order,
    }
}

/// Han characters that are worth transliterating.
fn is_han(ch: char) -> bool {
    (0x3400..=0x9fff).contains(&(ch as u32))
}

/// True when every character lowercases to itself, so `str::to_lowercase`
/// would return a copy of the same text. Checked per character without
/// allocating: the result decides whether an entry stores a second string.
fn lowercases_to_itself(name: &str) -> bool {
    name.chars().all(|ch| {
        let mut lowered = ch.to_lowercase();
        lowered.next() == Some(ch) && lowered.next().is_none()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_is_already_lowercase_stores_no_second_copy() {
        for name in ["readme.txt", "报告.txt", "2024-plan"] {
            let entry = Entry::new(
                PathBuf::from(format!("/dir/{name}")),
                String::new(),
                0,
                0,
                false,
            );
            assert!(
                entry.lower_name.is_none(),
                "{name} should not need a lowercase copy"
            );
            assert_eq!(entry.lower_name(), name);
        }
    }

    #[test]
    fn a_name_with_uppercase_or_accented_letters_stores_a_copy() {
        for (name, lowered) in [
            ("README.TXT", "readme.txt"),
            ("Report Final.PDF", "report final.pdf"),
            ("CAFÉ.txt", "café.txt"),
        ] {
            let entry = Entry::new(
                PathBuf::from(format!("/dir/{name}")),
                String::new(),
                0,
                0,
                false,
            );
            assert_eq!(entry.lower_name(), lowered);
        }
    }

    #[test]
    fn shared_dir_splits_the_two_forms() {
        let shared = SharedDir::new("/Users/Joe/Docs".to_string());
        assert_eq!(shared.dir(), "/Users/Joe/Docs");
        assert_eq!(shared.lower_dir(), "/users/joe/docs");
        // 空目录（相对路径的父目录）两种形式都为空，路径拼接据此走分支。
        let empty = SharedDir::new(String::new());
        assert_eq!(empty.dir(), "");
        assert_eq!(empty.lower_dir(), "");
    }
}

/// Progress of a scan. `visited` counts indexed entries, `skipped` counts
/// entries or directories that were rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub visited: usize,
    pub skipped: usize,
    pub current: PathBuf,
}

/// Lowercased extension including the dot, matching Go's `filepath.Ext` for the
/// inputs this crate cares about (`""` when the name has no dot).
pub(crate) fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(index) => name[index..].to_lowercase(),
        None => String::new(),
    }
}
