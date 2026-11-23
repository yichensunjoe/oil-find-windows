//! Local document extraction and literal substring search for approved folders.
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Cursor, Read};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_DOC_BYTES: usize = 2 * 1024 * 1024;
const MAX_UNITS: usize = 30_000;
const MAX_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 20_000;
const MAX_TOTAL_UNITS: usize = 400_000;
const PREVIEW_CONTEXT_BYTES: usize = 12_000;
pub const MAX_CACHE_BYTES: usize = 160 * 1024 * 1024;
const CACHE_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    Pdf,
    Word,
    Text,
}

impl Format {
    fn for_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "pdf" => Some(Self::Pdf),
            "docx" => Some(Self::Word),
            "txt" | "md" | "markdown" => Some(Self::Text),
            _ => None,
        }
    }

    pub fn location(self, number: usize) -> String {
        let unit = match self {
            Self::Pdf => '页',
            Self::Word => '段',
            Self::Text => '行',
        };
        format!("第 {number} {unit}")
    }
}

#[derive(Serialize, Deserialize)]
pub struct ContentIndex {
    version: u8,
    roots: Vec<PathBuf>,
    documents: Vec<Document>,
    pub skipped: usize,
}

#[derive(Serialize, Deserialize)]
pub struct Document {
    pub path: PathBuf,
    pub format: Format,
    stamp: FileStamp,
    units: Vec<String>,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
struct FileStamp {
    bytes: u64,
    modified: Duration,
    before_epoch: bool,
}

impl FileStamp {
    fn from_metadata(metadata: &fs::Metadata) -> io::Result<Self> {
        let time = metadata.modified()?.duration_since(UNIX_EPOCH);
        let before_epoch = time.is_err();
        let modified = time.unwrap_or_else(|err| err.duration());
        Ok(Self {
            bytes: metadata.len(),
            modified,
            before_epoch,
        })
    }
}

#[derive(Clone)]
pub struct Hit {
    document: usize,
    pub count: usize,
    pub location: usize,
    pub snippet: String,
}

pub struct Preview {
    pub text: String,
    pub selection: Range<usize>,
    pub location: usize,
}

impl ContentIndex {
    pub fn build(
        cancel: &AtomicBool,
        roots: &[PathBuf],
        approved_volumes: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        // Reuse the scan's root validation, protected directories, traversal and
        // cancellation. Reading starts only with the files it has accepted.
        let scan = oilfind_core::scan(cancel, roots, |_| {}).map_err(|err| err.to_string())?;
        let mut index = Self {
            version: CACHE_VERSION,
            roots: roots.to_vec(),
            documents: Vec::new(),
            skipped: 0,
        };
        let mut text_bytes = 0;
        let mut unit_count = 0;
        for entry in &scan.entries {
            if cancel.load(Relaxed) {
                return Err("正文索引已取消".into());
            }
            if entry.is_dir {
                continue;
            }
            let path = entry.path();
            let Some(format) = Format::for_path(&path) else {
                continue;
            };
            if index.len() >= MAX_DOCUMENTS
                || text_bytes >= MAX_TEXT_BYTES
                || unit_count >= MAX_TOTAL_UNITS
                || entry.size > MAX_FILE_BYTES as u64
                || !roots.iter().any(|root| under_root(root, &path))
            {
                index.skipped += 1;
                continue;
            }
            let Ok((raw, stamp)) = read_source(&path, approved_volumes) else {
                index.skipped += 1;
                continue;
            };
            let Some(units) = extract(format, &raw, cancel) else {
                index.skipped += 1;
                continue;
            };
            let size = units.iter().map(String::len).sum::<usize>();
            if text_bytes + size > MAX_TEXT_BYTES || unit_count + units.len() > MAX_TOTAL_UNITS {
                index.skipped += 1;
                continue;
            }
            text_bytes += size;
            unit_count += units.len();
            index.documents.push(Document {
                path,
                format,
                stamp,
                units,
            });
        }
        if cancel.load(Relaxed) {
            return Err("正文索引已取消".into());
        }
        Ok(index)
    }

    pub fn from_cache(data: &[u8], roots: &[PathBuf]) -> Result<Self, String> {
        if data.len() > MAX_CACHE_BYTES || roots.is_empty() {
            return Err("正文索引缓存无效".into());
        }
        let index: Self = serde_json::from_slice(data).map_err(|err| err.to_string())?;
        if index.version != CACHE_VERSION
            || index.roots != roots
            || index.len() > MAX_DOCUMENTS
            || index.documents.iter().any(|doc| {
                Format::for_path(&doc.path) != Some(doc.format)
                    || !roots.iter().any(|root| under_root(root, &doc.path))
                    || !valid_units(&doc.units)
            })
            || index
                .documents
                .iter()
                .map(|doc| doc.units.len())
                .sum::<usize>()
                > MAX_TOTAL_UNITS
            || index
                .documents
                .iter()
                .flat_map(|doc| &doc.units)
                .map(String::len)
                .sum::<usize>()
                > MAX_TEXT_BYTES
        {
            return Err("正文缓存与当前授权范围不匹配或已损坏".into());
        }
        Ok(index)
    }

    pub fn len(&self) -> usize {
        self.documents.len()
    }

    pub fn document(&self, hit: &Hit) -> &Document {
        &self.documents[hit.document]
    }

    pub fn search(
        &self,
        term: &str,
        limit: usize,
        compare: impl FnMut(&Hit, &Hit) -> Ordering,
    ) -> (Vec<Hit>, usize) {
        if term.is_empty() {
            return (Vec::new(), 0);
        }
        let mut hits = Vec::new();
        for (document, doc) in self.documents.iter().enumerate() {
            let mut matches = doc.matches(term);
            let Some((unit, offset)) = matches.next() else {
                continue;
            };
            hits.push(Hit {
                document,
                count: 1 + matches.count(),
                location: doc.location(unit, offset),
                snippet: snippet(&doc.units[unit], offset, term),
            });
        }
        let total = hits.len();
        hits.sort_by(compare);
        hits.truncate(limit);
        (hits, total)
    }
}

impl Document {
    pub fn name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
    }

    pub fn matches_metadata(&self, metadata: &fs::Metadata) -> bool {
        metadata.is_file()
            && FileStamp::from_metadata(metadata).is_ok_and(|stamp| self.stamp == stamp)
    }

    fn matches<'a>(&'a self, term: &'a str) -> impl Iterator<Item = (usize, usize)> + 'a {
        self.units.iter().enumerate().flat_map(move |(unit, text)| {
            text.match_indices(term)
                .map(move |(offset, _)| (unit, offset))
        })
    }

    fn location(&self, unit: usize, offset: usize) -> usize {
        if self.format == Format::Text {
            1 + self.units[unit].as_bytes()[..offset]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count()
        } else {
            unit + 1
        }
    }

    pub fn preview(&self, term: &str, ordinal: usize) -> Option<Preview> {
        if term.is_empty() {
            return None;
        }
        let (unit, offset) = self.matches(term).nth(ordinal)?;
        let text = &self.units[unit];
        let matched_end = offset + term.len();
        let start = text.floor_char_boundary(offset.saturating_sub(PREVIEW_CONTEXT_BYTES));
        let end = text.ceil_char_boundary((matched_end + PREVIEW_CONTEXT_BYTES).min(text.len()));
        let mut before = String::new();
        if start > 0 {
            before.push_str("…\n");
        } else if self.format == Format::Word && unit > 0 {
            let previous = &self.units[unit - 1];
            let tail = previous.ceil_char_boundary(previous.len().saturating_sub(1024));
            before.push_str(&previous[tail..]);
            before.push_str("\n\n");
        }
        before.push_str(&text[start..offset]);
        let selection_start = edit_length(&before);
        before.push_str(&text[offset..end]);
        if end < text.len() {
            before.push_str("\n…");
        } else if self.format == Format::Word {
            if let Some(next) = self.units.get(unit + 1) {
                before.push_str("\n\n");
                before.push_str(&next[..next.floor_char_boundary(next.len().min(1024))]);
            }
        }
        Some(Preview {
            text: before.replace('\n', "\r\n"),
            selection: selection_start..selection_start + edit_length(term),
            location: self.location(unit, offset),
        })
    }
}

fn edit_length(text: &str) -> usize {
    text.encode_utf16().count() + text.bytes().filter(|&byte| byte == b'\n').count()
}

fn snippet(text: &str, offset: usize, term: &str) -> String {
    let end = offset + term.len();
    let preceding = text[..offset].char_indices().rev().nth(15);
    let following = text[end..].char_indices().nth(22);
    let start = preceding.map_or(0, |(position, _)| position);
    let stop = following.map_or(text.len(), |(position, _)| end + position);
    let lead = if start > 0 { "…" } else { "" };
    let tail = if stop < text.len() { "…" } else { "" };
    format!(
        "{lead}{}【{term}】{}{tail}",
        &text[start..offset],
        &text[end..stop]
    )
    .replace(['\r', '\n', '\t'], " ")
}

fn valid_units(units: &[String]) -> bool {
    !units.is_empty()
        && units.len() <= MAX_UNITS
        && units.iter().map(String::len).sum::<usize>() <= MAX_DOC_BYTES
        && units.iter().all(|unit| !unit.contains(['\0', '\r']))
}

fn extract(format: Format, raw: &[u8], cancel: &AtomicBool) -> Option<Vec<String>> {
    let units = match format {
        Format::Pdf => pdf_units(raw, cancel)?,
        Format::Word => word_units(raw)?,
        Format::Text => vec![normalize_text(&text_file(raw)?)],
    };
    (valid_units(&units) && units.iter().any(|unit| !unit.trim().is_empty())).then_some(units)
}

fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|&ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect()
}

fn text_file(bytes: &[u8]) -> Option<std::borrow::Cow<'_, str>> {
    let (encoding, bom) = encoding_rs::Encoding::for_bom(bytes).unwrap_or((encoding_rs::UTF_8, 0));
    let bytes = &bytes[bom..];
    let decoded = encoding.decode_without_bom_handling_and_without_replacement(bytes);
    let text = decoded.or_else(|| {
        if bom > 0 {
            None
        } else {
            encoding_rs::GB18030.decode_without_bom_handling_and_without_replacement(bytes)
        }
    })?;
    (!text.contains('\0')).then_some(text)
}

fn pdf_units(bytes: &[u8], cancel: &AtomicBool) -> Option<Vec<String>> {
    const PDF_STREAM_LIMIT: usize = 8 * 1024 * 1024;
    const MAX_PAGES: usize = 4096;
    let options = lopdf::LoadOptions::with_max_decompressed_size(PDF_STREAM_LIMIT);
    let pdf = lopdf::Document::load_mem_with_options(bytes, options).ok()?;
    if pdf.is_encrypted() {
        return None;
    }
    let page_count = pdf.page_iter().take(MAX_PAGES + 1).count();
    if page_count > MAX_PAGES {
        return None;
    }
    let mut total = 0;
    let mut units = Vec::with_capacity(page_count);
    for page in 1..=page_count as u32 {
        if cancel.load(Relaxed) {
            return None;
        }
        let page_text = pdf
            .extract_text_with_limit(&[page], PDF_STREAM_LIMIT)
            .ok()?;
        let page_text = normalize_text(&page_text);
        total += page_text.len();
        if total > MAX_DOC_BYTES {
            return None;
        }
        units.push(page_text);
    }
    Some(units)
}

fn word_units(bytes: &[u8]) -> Option<Vec<String>> {
    const WORD_TRANSITIONAL: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    const WORD_STRICT: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    let xml = archive.by_name("word/document.xml").ok()?;
    if xml.size() > MAX_XML_BYTES as u64 {
        return None;
    }
    let mut raw = Vec::new();
    xml.take(MAX_XML_BYTES as u64 + 1)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() > MAX_XML_BYTES {
        return None;
    }
    let xml = text_file(&raw)?;
    let tree = roxmltree::Document::parse_with_options(
        &xml,
        roxmltree::ParsingOptions {
            nodes_limit: 300_000,
            ..Default::default()
        },
    )
    .ok()?;
    let body = tree.root_element().children().find(|node| {
        matches!(
            node.tag_name().namespace(),
            Some(WORD_TRANSITIONAL | WORD_STRICT)
        ) && node.has_tag_name("body")
    })?;
    let namespace = body.tag_name().namespace()?;
    let mut units = Vec::<String>::new();
    let mut paragraphs = HashMap::new();
    for node in body
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().namespace() == Some(namespace))
    {
        let name = node.tag_name().name();
        if name == "p" {
            if units.len() >= MAX_UNITS {
                return None;
            }
            paragraphs.insert(node.id(), units.len());
            units.push(String::new());
            continue;
        }
        if !matches!(name, "t" | "tab" | "br" | "cr") {
            continue;
        }
        let Some(paragraph) = node
            .ancestors()
            .find(|ancestor| ancestor.has_tag_name((namespace, "p")))
        else {
            continue;
        };
        let text = &mut units[paragraphs[&paragraph.id()]];
        match name {
            "t" => text.push_str(node.text().unwrap_or_default()),
            "tab" => text.push('\t'),
            _ => text.push('\n'),
        }
    }
    for unit in &mut units {
        *unit = normalize_text(unit);
    }
    Some(units)
}

pub fn read_local_file(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    read_limited(&open_local_file(path, None)?, limit)
}

fn read_source(
    path: &Path,
    approved_volumes: &BTreeMap<String, String>,
) -> io::Result<(Vec<u8>, FileStamp)> {
    #[cfg(windows)]
    let volume_path = {
        let text = path
            .to_str()
            .ok_or_else(|| io::Error::other("文件路径无效"))?;
        let drive = text
            .get(..2)
            .ok_or_else(|| io::Error::other("文件盘符无效"))?;
        let drive = format!("{}\\", drive.to_ascii_uppercase());
        let volume = approved_volumes
            .get(&drive)
            .ok_or_else(|| io::Error::other("文件所在磁盘未经确认"))?;
        let rest = text
            .get(3..)
            .ok_or_else(|| io::Error::other("文件路径无效"))?;
        Some(format!("{volume}{rest}"))
    };
    #[cfg(not(windows))]
    let volume_path: Option<String> = {
        let _ = approved_volumes;
        None
    };
    let file = open_local_file(path, volume_path.as_deref())?;
    let stamp = FileStamp::from_metadata(&file.metadata()?)?;
    let data = read_limited(&file, MAX_FILE_BYTES)?;
    if stamp.bytes != data.len() as u64 || stamp != FileStamp::from_metadata(&file.metadata()?)? {
        return Err(io::Error::other("读取正文时文件发生变化"));
    }
    Ok((data, stamp))
}

fn read_limited(file: &File, limit: usize) -> io::Result<Vec<u8>> {
    if file.metadata()?.len() > limit as u64 {
        return Err(io::Error::other("文件超出大小限制"));
    }
    let mut data = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut data)?;
    if data.len() > limit {
        return Err(io::Error::other("文件超出大小限制"));
    }
    Ok(data)
}

#[cfg(windows)]
fn open_local_file(path: &Path, volume_path: Option<&str>) -> io::Result<File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_NO_RECALL,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED, VOLUME_NAME_GUID,
    };

    const UNSAFE_ATTRIBUTES: u32 = FILE_ATTRIBUTE_REPARSE_POINT
        | FILE_ATTRIBUTE_OFFLINE
        | FILE_ATTRIBUTE_RECALL_ON_OPEN
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_attributes() & UNSAFE_ATTRIBUTES != 0 {
        return Err(io::Error::other("不读取链接或离线文件"));
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & UNSAFE_ATTRIBUTES != 0 {
        return Err(io::Error::other("不读取链接或离线文件"));
    }

    // Validate the opened handle before reading any bytes. This prevents a
    // junction swapped in after the metadata scan from redirecting a body read.
    let mut buffer = [0; 512];
    let flags = FILE_NAME_NORMALIZED
        | if volume_path.is_some() {
            VOLUME_NAME_GUID
        } else {
            0
        };
    let read_path = |buffer: &mut [u16]| unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            flags,
        ) as usize
    };
    let length = read_path(&mut buffer);
    let resolved = if length >= buffer.len() && length <= 32_768 {
        let mut large = vec![0; length + 1];
        let length = read_path(&mut large);
        if length == 0 || length >= large.len() {
            return Err(io::Error::other("无法验证已打开的文件"));
        }
        String::from_utf16_lossy(&large[..length])
    } else if length > 0 && length < buffer.len() {
        String::from_utf16_lossy(&buffer[..length])
    } else {
        return Err(io::Error::other("无法验证已打开的文件"));
    };
    let expected = volume_path.map_or(path, Path::new);
    let resolved = if volume_path.is_some() {
        Path::new(&resolved)
    } else {
        Path::new(resolved.strip_prefix(r"\\?\").unwrap_or(&resolved))
    };
    if !crate::platform::within_root(resolved, expected)
        || !crate::platform::within_root(expected, resolved)
    {
        return Err(io::Error::other("文件位置已变化"));
    }
    Ok(file)
}

#[cfg(not(windows))]
fn open_local_file(path: &Path, _: Option<&str>) -> io::Result<File> {
    if !fs::symlink_metadata(path)?.is_file() || fs::canonicalize(path)? != path {
        return Err(io::Error::other("文件位置已变化"));
    }
    File::open(path)
}

fn under_root(root: &Path, path: &Path) -> bool {
    let safe = path.is_absolute()
        && path.to_str().is_some()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir));
    #[cfg(windows)]
    return safe && crate::platform::within_root(root, path);
    #[cfg(not(windows))]
    return safe && path.starts_with(root);
}
