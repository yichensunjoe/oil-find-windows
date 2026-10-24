//! Cache persistence.
//!
//! Two formats are readable, one is written:
//!
//! * **Write (binary, current).** A file that starts with the magic
//!   `b"OILF1\0\0\0"` holds a bincode document: `version`, `roots`, `built_at`,
//!   a table of distinct parent `directories` and the `entries` that refer to
//!   them by index. Storing each directory once instead of repeating its
//!   prefix in every path is what keeps a 1.5M-entry index around 70 MB
//!   instead of 300 MB.
//! * **Read only (JSON, legacy).** A file that does not start with the magic
//!   is parsed as the document the Go implementation writes, so an existing
//!   cache survives an in-place upgrade:
//!
//!   ```json
//!   {"version":1,"roots":["C:\\Users\\me\\Documents"],
//!    "builtAt":"2026-10-24T05:09:49Z",
//!    "entries":[{"path":"...","name":"a.txt","size":3,"modified":1761287389,"isDir":false}]}
//!   ```
//!
//! Reads are streamed and bounded: the entry count declared by the file is
//! rejected before anything is allocated, every entry is validated on the way
//! in, and the reader is capped at [`MAX_CACHE_BYTES`]. Writes go to a sibling
//! temporary file, are size-capped while encoding, fsynced and then atomically
//! renamed, so a failed or cancelled save always leaves the previous cache
//! untouched.

use crate::entry::{Entry, Index};
use crate::error::Error;
use crate::platform;
use crate::scan::{MAX_ENTRIES, MAX_ROOTS};
use crate::time;
use bincode::Options;
use rayon::prelude::*;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Hard ceiling on the cache file size.
pub const MAX_CACHE_BYTES: u64 = 512 << 20;

/// Cache format version. Bumping this forces every client to re-index.
const CACHE_VERSION: u32 = 1;

/// Prefix that marks a binary cache. Anything else is read as legacy JSON.
const MAGIC: [u8; 8] = *b"OILF1\0\0\0";

const MAX_PATH_LEN: usize = 32_767;
const MAX_NAME_LEN: usize = 1_024;

/// Buffered I/O size for the cache file: one megabyte keeps the encoder from
/// issuing a syscall per varint without holding a large copy of the index.
const WRITE_BUFFER_BYTES: usize = 1 << 20;

/// Upper bound on how many elements a single sequence is allowed to declare
/// before it is allocated, so a hostile header cannot request gigabytes.
const MAX_DECLARED_ITEMS: usize = MAX_ENTRIES;

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
        let (directories, index) = self.directory_table();

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
            // bincode emits varints a couple of bytes at a time; without this
            // buffer every one of them would be its own `write` syscall.
            let buffered = io::BufWriter::with_capacity(WRITE_BUFFER_BYTES, file);
            let mut writer = LimitedWriter::new(buffered, MAX_CACHE_BYTES);
            writer.write_all(&MAGIC)?;
            let document = BinaryCacheRef {
                version: CACHE_VERSION,
                roots: &self.roots,
                built_at: self.built_at,
                directories: &directories,
                entries: EntriesRef {
                    entries: &self.entries,
                    index: &index,
                },
            };
            let encoded = binary_options().serialize_into(&mut writer, &document);
            if let Err(err) = encoded {
                return Err(if writer.exceeded {
                    Error::CacheTooLarge
                } else {
                    Error::Io(io::Error::other(err.to_string()))
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

    /// Reads and validates a cache written by [`Index::save`], or the legacy
    /// JSON document written by the Go implementation.
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
        let mut reader =
            BufReader::with_capacity(WRITE_BUFFER_BYTES, file.take(MAX_CACHE_BYTES + 1));
        let mut magic = [0u8; MAGIC.len()];
        match reader.read_exact(&mut magic) {
            Ok(()) => {}
            // Too short to be either format: let the JSON parser reject it.
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {}
            Err(err) => return Err(Error::Io(err)),
        }
        if magic == MAGIC {
            return load_binary(reader);
        }
        // Legacy JSON: the magic probe consumed bytes, so reopen the file.
        let file = File::open(path)?;
        let reader = BufReader::with_capacity(WRITE_BUFFER_BYTES, file.take(MAX_CACHE_BYTES + 1));
        load_json(reader)
    }

    /// Maps every entry to the index of its parent directory in `directories`.
    ///
    /// Paths under one directory share a long prefix; storing that prefix once
    /// is what makes the binary cache roughly a quarter of the JSON one.
    fn directory_table(&self) -> (Vec<&Path>, HashMap<&Path, u32>) {
        let mut index: HashMap<&Path, u32> = HashMap::with_capacity(self.entries.len() / 8);
        let mut directories: Vec<&Path> = Vec::new();
        for entry in &self.entries {
            let parent = entry.path.parent().unwrap_or_else(|| Path::new(""));
            index.entry(parent).or_insert_with(|| {
                directories.push(parent);
                // Entries are capped at MAX_ENTRIES, well below u32::MAX.
                (directories.len() - 1) as u32
            });
        }
        (directories, index)
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

/// Binary document, borrowed from the index being saved.
///
/// `directories` holds each distinct parent path once and `entries` refers to
/// it by index, which is the whole point of the format: a 1.5M-entry index
/// repeats the same `/Users/…/Documents` prefix a hundred thousand times in
/// JSON.
#[derive(Serialize)]
struct BinaryCacheRef<'a> {
    version: u32,
    roots: &'a [PathBuf],
    built_at: i64,
    directories: &'a [&'a Path],
    entries: EntriesRef<'a>,
}

/// Serializes entries as `(directory index, name, size, modified, is_dir)`.
///
/// Entries are written one at a time, so saving never builds a second copy of
/// the index in memory.
struct EntriesRef<'a> {
    entries: &'a [Entry],
    index: &'a HashMap<&'a Path, u32>,
}

impl Serialize for EntriesRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.entries.len()))?;
        for entry in self.entries {
            let parent = entry.path.parent().unwrap_or_else(|| Path::new(""));
            let directory = self.index.get(parent).copied().unwrap_or_default();
            seq.serialize_element(&BinaryEntry {
                directory,
                name: &entry.name,
                size: entry.size,
                modified: entry.modified,
                is_dir: entry.is_dir,
            })?;
        }
        seq.end()
    }
}

#[derive(Serialize)]
struct BinaryEntry<'a> {
    directory: u32,
    name: &'a str,
    size: u64,
    modified: i64,
    is_dir: bool,
}

/// Binary document read back from disk. Field order must match
/// [`BinaryCacheRef`] and [`StoredEntry`]: bincode is positional.
#[derive(Deserialize)]
struct BinaryCache {
    version: u32,
    roots: Vec<PathBuf>,
    built_at: i64,
    #[serde(deserialize_with = "deserialize_directories")]
    directories: Vec<PathBuf>,
    #[serde(deserialize_with = "deserialize_entries")]
    entries: Vec<StoredEntry>,
}

/// Same field order as [`BinaryEntry`].
#[derive(Deserialize)]
struct StoredEntry {
    directory: u32,
    name: String,
    size: u64,
    modified: i64,
    is_dir: bool,
}

impl BinaryCache {
    /// Rebuilds the index, rejecting anything the writer would not produce.
    fn into_index(self) -> Result<Index, Error> {
        if self.version != CACHE_VERSION {
            return Err(Error::CacheCorrupt(
                "索引版本不兼容，请重新索引".to_string(),
            ));
        }
        if self.roots.len() > MAX_ROOTS {
            return Err(Error::CacheCorrupt("索引目录数量超出上限".to_string()));
        }
        for root in &self.roots {
            validate_root(root).map_err(|_| Error::CacheCorrupt("索引目录损坏".to_string()))?;
        }
        // Rebuilding the derived search fields (lowercase + pinyin) costs more
        // than decoding, and every entry is independent, so it runs on all
        // cores. The results are written into a pre-sized vector in place:
        // collecting into a fresh one would hold two copies of every entry at
        // once, which is a few hundred megabytes on a large index.
        let directories = self.directories;
        let mut entries: Vec<Entry> =
            vec![Entry::new(PathBuf::new(), String::new(), 0, 0, false); self.entries.len()];
        entries
            .par_iter_mut()
            .zip(self.entries.into_par_iter())
            .try_for_each(|(slot, stored)| -> Result<(), Error> {
                let directory = directories
                    .get(stored.directory as usize)
                    .ok_or_else(|| Error::CacheCorrupt("索引目录索引越界".to_string()))?;
                let entry = Entry::new(
                    directory.join(&stored.name),
                    stored.name,
                    stored.size,
                    stored.modified,
                    stored.is_dir,
                );
                validate_entry(&entry)
                    .map_err(|_| Error::CacheCorrupt("索引条目损坏".to_string()))?;
                *slot = entry;
                Ok(())
            })?;
        Ok(Index {
            entries,
            roots: self.roots,
            built_at: self.built_at,
        })
    }
}

/// Little-endian, variable-length integers: lengths and timestamps are the
/// bulk of the file, and a varint turns an 8-byte length into one or three.
fn binary_options() -> impl Options {
    bincode::DefaultOptions::new()
        .with_little_endian()
        .with_varint_encoding()
}

fn deserialize_directories<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<PathBuf>, D::Error> {
    bounded_sequence::<D, PathBuf, _>(deserializer, "索引目录", |root| {
        validate_root(root).map_err(|_| Error::InvalidRoot)
    })
}

fn deserialize_entries<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<StoredEntry>, D::Error> {
    bounded_sequence::<D, StoredEntry, _>(deserializer, "索引条目", |entry| {
        let valid = !entry.name.is_empty()
            && entry.name.len() <= MAX_NAME_LEN
            && !entry.name.contains('\0');
        if valid {
            Ok(())
        } else {
            Err(Error::InvalidEntry)
        }
    })
}

/// Decodes a sequence after checking its declared length against
/// [`MAX_DECLARED_ITEMS`], so a corrupt header is rejected before the vector
/// that backs it is allocated. Elements are still validated one by one.
fn bounded_sequence<'de, D, T, V>(
    deserializer: D,
    expected: &'static str,
    validate: V,
) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
    V: Fn(&T) -> Result<(), Error>,
{
    struct Bounded<T, V> {
        expected: &'static str,
        validate: V,
        marker: std::marker::PhantomData<T>,
    }

    impl<'de, T: Deserialize<'de>, V: Fn(&T) -> Result<(), Error>> Visitor<'de> for Bounded<T, V> {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "a list of {}", self.expected)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            let declared = seq.size_hint().unwrap_or(0);
            if declared > MAX_DECLARED_ITEMS {
                return Err(de::Error::custom("索引条目超出安全上限"));
            }
            // Bounded by MAX_ENTRIES, and never more than the file can hold.
            let mut items: Vec<T> = Vec::with_capacity(declared);
            while let Some(item) = seq.next_element::<T>()? {
                if items.len() >= MAX_DECLARED_ITEMS {
                    return Err(de::Error::custom("索引条目超出安全上限"));
                }
                (self.validate)(&item).map_err(de::Error::custom)?;
                items.push(item);
            }
            Ok(items)
        }
    }

    deserializer.deserialize_seq(Bounded {
        expected,
        validate,
        marker: std::marker::PhantomData,
    })
}

/// Decodes the binary document and refuses trailing bytes, which would mean
/// the file was written by a different layout.
fn load_binary<R: BufRead>(mut reader: R) -> Result<Index, Error> {
    let document: BinaryCache = binary_options()
        .deserialize_from(&mut reader)
        .map_err(|err| Error::CacheCorrupt(err.to_string()))?;
    let trailing = reader.fill_buf().map_err(Error::Io)?;
    if !trailing.is_empty() {
        return Err(Error::CacheCorrupt("索引缓存包含多余数据".to_string()));
    }
    document.into_index()
}

/// Decodes the legacy JSON document the Go implementation writes.
fn load_json<R: Read>(reader: R) -> Result<Index, Error> {
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let index = Index::deserialize(&mut deserializer)
        .map_err(|err| Error::CacheCorrupt(err.to_string()))?;
    deserializer
        .end()
        .map_err(|err| Error::CacheCorrupt(err.to_string()))?;
    Ok(index)
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
