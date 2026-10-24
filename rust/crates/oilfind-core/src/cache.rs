//! Cache persistence.
//!
//! The on-disk format is the same JSON document the Go implementation writes,
//! so an existing cache survives an in-place upgrade:
//!
//! ```json
//! {"version":1,"roots":["C:\\Users\\me\\Documents"],
//!  "builtAt":"2026-10-24T05:09:49Z",
//!  "entries":[{"path":"...","name":"a.txt","size":3,"modified":1761287389,"isDir":false}]}
//! ```
//!
//! Reads are streamed: the entry array is validated one element at a time and
//! aborts as soon as it exceeds [`MAX_ENTRIES`], so a hostile file cannot force
//! a multi-gigabyte allocation. Writes go to a sibling temporary file, are
//! size-capped, fsynced and then atomically renamed, so a failed or cancelled
//! save always leaves the previous cache untouched.

use crate::entry::{Entry, Index};
use crate::error::Error;
use crate::platform;
use crate::scan::{MAX_ENTRIES, MAX_ROOTS};
use crate::time;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Hard ceiling on the cache file size.
pub const MAX_CACHE_BYTES: u64 = 512 << 20;

/// Cache format version. Bumping this forces every client to re-index.
const CACHE_VERSION: u32 = 1;

const MAX_PATH_LEN: usize = 32_767;
const MAX_NAME_LEN: usize = 1_024;

/// Used by [`Index::save`], which is never cancelled.
static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

impl Index {
    /// Writes the index to `path`, replacing any previous cache atomically.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        self.save_cancellable(&NEVER_CANCELLED, path)
    }

    /// Like [`Index::save`], but abandons the write when `cancel` is set.
    ///
    /// Cancellation is checked before encoding, after the temporary file is
    /// fsynced and before the rename, so the previous cache always survives.
    pub fn save_cancellable(&self, cancel: &AtomicBool, path: &Path) -> Result<(), Error> {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        self.validate_for_save()?;

        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        let directory = parent.unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(directory)?;

        // Dropping `temp` removes the partial file on every error path.
        let mut temp = TempFile::create(directory)?;
        {
            let file = temp
                .file
                .as_mut()
                .ok_or_else(|| Error::Io(io::Error::other("临时文件不可用")))?;
            let mut writer = LimitedWriter::new(file, MAX_CACHE_BYTES);
            let encoded = serde_json::to_writer(&mut writer, self);
            if let Err(err) = encoded {
                return Err(if writer.exceeded {
                    Error::CacheTooLarge
                } else {
                    Error::Io(err.into())
                });
            }
            writer.flush()?;
        }
        if let Some(file) = temp.file.as_ref() {
            file.sync_all()?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        platform::replace_file(&temp.path, path)?;
        Ok(())
    }

    /// Reads and validates a cache written by [`Index::save`].
    pub fn load(path: &Path) -> Result<Index, Error> {
        let metadata = fs::symlink_metadata(path)?;
        if platform::is_link_or_reparse(&metadata) {
            return Err(Error::CacheUnsupported("索引缓存不能是链接或重解析点"));
        }
        if !metadata.is_file() {
            return Err(Error::CacheUnsupported("索引缓存必须是普通文件"));
        }
        if metadata.len() > MAX_CACHE_BYTES {
            return Err(Error::CacheTooLarge);
        }
        let file = File::open(path)?;
        // A file that grew between the stat and the read cannot exceed the cap.
        let reader = BufReader::new(file.take(MAX_CACHE_BYTES + 1));
        let mut deserializer = serde_json::Deserializer::from_reader(reader);
        let index = Index::deserialize(&mut deserializer)
            .map_err(|err| Error::CacheCorrupt(err.to_string()))?;
        deserializer
            .end()
            .map_err(|err| Error::CacheCorrupt(err.to_string()))?;
        Ok(index)
    }

    fn validate_for_save(&self) -> Result<(), Error> {
        if self.entries.len() > MAX_ENTRIES {
            return Err(Error::EntryLimit);
        }
        if self.roots.len() > MAX_ROOTS {
            return Err(Error::TooManyRoots);
        }
        for entry in &self.entries {
            validate_entry(entry)?;
        }
        for root in &self.roots {
            validate_root(root)?;
        }
        Ok(())
    }
}

/// Wire form: the same document [`Index::load`] reads back, so an `Index`
/// always round-trips through serde.
impl Serialize for Index {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Index", 4)?;
        state.serialize_field("version", &CACHE_VERSION)?;
        state.serialize_field("roots", &self.roots)?;
        state.serialize_field("builtAt", &Rfc3339(self.built_at))?;
        state.serialize_field("entries", &self.entries)?;
        state.end()
    }
}

/// Serializes Unix seconds as the RFC 3339 string the Go implementation writes.
struct Rfc3339(i64);

impl Serialize for Rfc3339 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&time::format_rfc3339(self.0))
    }
}

fn validate_entry(entry: &Entry) -> Result<(), Error> {
    let path = entry.path.to_string_lossy();
    let invalid = path.is_empty()
        || entry.name.is_empty()
        || path.len() > MAX_PATH_LEN
        || entry.name.len() > MAX_NAME_LEN
        || path.contains('\0')
        || entry.name.contains('\0');
    if invalid {
        return Err(Error::InvalidEntry);
    }
    Ok(())
}

fn validate_root(root: &Path) -> Result<(), Error> {
    let text = root.to_string_lossy();
    if text.is_empty() || text.len() > MAX_PATH_LEN || text.contains('\0') {
        return Err(Error::InvalidRoot);
    }
    Ok(())
}

/// Streaming reader for the cache document.
impl<'de> Deserialize<'de> for Index {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Index, D::Error> {
        deserializer.deserialize_map(CacheVisitor)
    }
}

struct CacheVisitor;

impl<'de> Visitor<'de> for CacheVisitor {
    type Value = Index;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an oilfind index cache object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Index, A::Error> {
        let mut version: Option<u32> = None;
        let mut roots: Option<Vec<PathBuf>> = None;
        let mut built_at = 0i64;
        let mut entries: Option<Vec<Entry>> = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "version" => {
                    if version.is_some() {
                        return Err(de::Error::custom("索引包含重复字段"));
                    }
                    version = Some(map.next_value()?);
                }
                "roots" => {
                    if roots.is_some() {
                        return Err(de::Error::custom("索引包含重复字段"));
                    }
                    roots = Some(map.next_value::<RootList>()?.0);
                }
                "builtAt" => built_at = map.next_value::<BuiltAt>()?.0,
                "entries" => {
                    if entries.is_some() {
                        return Err(de::Error::custom("索引包含重复字段"));
                    }
                    entries = Some(map.next_value::<EntryList>()?.0);
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        match version {
            Some(CACHE_VERSION) => {}
            _ => return Err(de::Error::custom("索引版本不兼容，请重新索引")),
        }
        Ok(Index {
            entries: entries.unwrap_or_default(),
            roots: roots.unwrap_or_default(),
            built_at,
        })
    }
}

/// Roots are decoded one at a time and capped before the vector grows.
struct RootList(Vec<PathBuf>);

impl<'de> Deserialize<'de> for RootList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(RootListVisitor)
    }
}

struct RootListVisitor;

impl<'de> Visitor<'de> for RootListVisitor {
    type Value = RootList;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a list of index roots")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<RootList, A::Error> {
        let mut roots: Vec<PathBuf> = Vec::new();
        while let Some(root) = seq.next_element::<PathBuf>()? {
            if roots.len() >= MAX_ROOTS {
                return Err(de::Error::custom("索引目录数量超出上限"));
            }
            validate_root(&root).map_err(de::Error::custom)?;
            roots.push(root);
        }
        Ok(RootList(roots))
    }
}

/// Entries are decoded one at a time so peak memory tracks the accepted
/// entries, never the declared length.
struct EntryList(Vec<Entry>);

impl<'de> Deserialize<'de> for EntryList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(EntryListVisitor)
    }
}

struct EntryListVisitor;

impl<'de> Visitor<'de> for EntryListVisitor {
    type Value = EntryList;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a list of index entries")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<EntryList, A::Error> {
        let mut entries: Vec<Entry> = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1024));
        while let Some(mut entry) = seq.next_element::<Entry>()? {
            if entries.len() >= MAX_ENTRIES {
                return Err(de::Error::custom("索引条目超出安全上限"));
            }
            validate_entry(&entry).map_err(de::Error::custom)?;
            entry.prepare();
            entries.push(entry);
        }
        Ok(EntryList(entries))
    }
}

/// Accepts both the RFC 3339 string the Go implementation writes and a plain
/// Unix timestamp.
struct BuiltAt(i64);

impl<'de> Deserialize<'de> for BuiltAt {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(BuiltAtVisitor)
    }
}

struct BuiltAtVisitor;

impl Visitor<'_> for BuiltAtVisitor {
    type Value = BuiltAt;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an RFC 3339 timestamp or Unix seconds")
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<BuiltAt, E> {
        Ok(BuiltAt(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<BuiltAt, E> {
        Ok(BuiltAt(value as i64))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<BuiltAt, E> {
        time::parse_rfc3339(value)
            .map(BuiltAt)
            .ok_or_else(|| E::custom("索引时间格式无效"))
    }
}

/// Refuses to encode more than `limit` bytes, so an oversized index is aborted
/// before the temporary file can reach the cache ceiling.
struct LimitedWriter<W> {
    inner: W,
    limit: u64,
    written: u64,
    exceeded: bool,
}

impl<W> LimitedWriter<W> {
    fn new(inner: W, limit: u64) -> Self {
        LimitedWriter {
            inner,
            limit,
            written: 0,
            exceeded: false,
        }
    }
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.written + buffer.len() as u64 > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("索引缓存超过 512MB 上限"));
        }
        let written = self.inner.write(buffer)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A temporary file in the destination directory that deletes itself unless it
/// was renamed into place.
struct TempFile {
    path: PathBuf,
    file: Option<File>,
}

impl TempFile {
    fn create(directory: &Path) -> Result<TempFile, Error> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        for _ in 0..64 {
            let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!(".oilfind-index-{}-{nanos}-{sequence}", std::process::id());
            let path = directory.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    let temp = TempFile {
                        path,
                        file: Some(file),
                    };
                    restrict_permissions(&temp.path)?;
                    return Ok(temp);
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(Error::Io(err)),
            }
        }
        Err(Error::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "无法创建临时索引文件",
        )))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        drop(self.file.take());
        // After a successful rename this simply fails with NotFound.
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), Error> {
    // The cache directory is already private to the user on Windows; the file
    // inherits the ACL of its parent.
    Ok(())
}
