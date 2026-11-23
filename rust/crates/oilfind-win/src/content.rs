//! Local document extraction and literal substring search for approved folders.
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Cursor, Read};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

mod inspection;
mod pdf;

use inspection::FileStamp;

pub(super) const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_DOC_BYTES: usize = 2 * 1024 * 1024;
const MAX_UNITS: usize = 30_000;
const MAX_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 20_000;
const MAX_TOTAL_UNITS: usize = 400_000;
const MAX_REMEMBERED_SKIPS: usize = 5_000;
const MAX_REMEMBERED_PATH_BYTES: usize = 1024 * 1024;
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
        let extension = path.extension()?.to_str()?;
        if extension.eq_ignore_ascii_case("pdf") {
            Some(Self::Pdf)
        } else if extension.eq_ignore_ascii_case("docx") {
            Some(Self::Word)
        } else if ["txt", "md", "markdown"]
            .iter()
            .any(|supported| extension.eq_ignore_ascii_case(supported))
        {
            Some(Self::Text)
        } else {
            None
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report: Option<RefreshReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skipped_sources: Vec<SkippedSource>,
}

#[derive(Serialize, Deserialize)]
pub struct Document {
    pub path: PathBuf,
    pub format: Format,
    stamp: FileStamp,
    #[serde(
        serialize_with = "serialize_units",
        deserialize_with = "deserialize_units"
    )]
    units: Arc<[String]>,
}

fn serialize_units<S: serde::Serializer>(
    units: &[String],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    units.serialize(serializer)
}

fn deserialize_units<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Arc<[String]>, D::Error> {
    Vec::<String>::deserialize(deserializer).map(Arc::from)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RefreshReport {
    pub reused: usize,
    pub extracted: usize,
    pub no_text: usize,
    pub too_large: usize,
    pub inaccessible: usize,
    pub extraction_failed: usize,
    pub unavailable: usize,
}

impl RefreshReport {
    fn record_skip(&mut self, reason: SkipReason) {
        *match reason {
            SkipReason::NoText => &mut self.no_text,
            SkipReason::TooLarge => &mut self.too_large,
            SkipReason::Inaccessible => &mut self.inaccessible,
            SkipReason::ExtractionFailed => &mut self.extraction_failed,
            SkipReason::Unavailable => &mut self.unavailable,
        } += 1;
    }

    fn skipped(self) -> usize {
        self.no_text
            .saturating_add(self.too_large)
            .saturating_add(self.inaccessible)
            .saturating_add(self.extraction_failed)
            .saturating_add(self.unavailable)
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
enum SkipReason {
    NoText,
    TooLarge,
    Inaccessible,
    ExtractionFailed,
    Unavailable,
}

impl SkipReason {
    fn from_io(err: &io::Error) -> Self {
        if err.kind() == io::ErrorKind::FileTooLarge {
            Self::TooLarge
        } else if inspection::is_inaccessible(err) {
            Self::Inaccessible
        } else {
            Self::Unavailable
        }
    }

    fn can_remember(self) -> bool {
        matches!(self, Self::NoText | Self::TooLarge | Self::ExtractionFailed)
    }
}

#[derive(Serialize, Deserialize)]
struct SkippedSource {
    path: PathBuf,
    stamp: FileStamp,
    reason: SkipReason,
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
    pub fn refresh(
        cancel: &AtomicBool,
        roots: &[PathBuf],
        approved_volumes: &BTreeMap<String, String>,
        previous: Option<&Self>,
    ) -> Result<Self, String> {
        check_cancelled(cancel)?;
        #[cfg(windows)]
        verify_volumes(roots, approved_volumes)?;
        // Reuse the scan's root validation, protected directories, traversal and
        // cancellation. Reading starts only with the files it has accepted.
        let scan = oilfind_core::scan(cancel, roots, |_| {}).map_err(|err| err.to_string())?;
        let previous = previous.filter(|index| index.roots == roots);
        let mut previously_indexed: HashMap<_, _> = previous
            .into_iter()
            .flat_map(|index| &index.documents)
            .map(|doc| (doc.path.as_path(), doc))
            .collect();
        let previously_skipped: HashMap<_, _> = previous
            .into_iter()
            .flat_map(|index| &index.skipped_sources)
            .map(|source| (source.path.as_path(), source))
            .collect();
        let mut report = RefreshReport::default();
        let mut documents = Vec::with_capacity(previous.map_or(0, Self::len));
        let mut skipped_sources = Vec::new();
        let mut remembered_path_bytes = 0;
        let mut text_bytes = 0;
        let mut unit_count = 0;
        for entry in &scan.entries {
            check_cancelled(cancel)?;
            if entry.is_dir {
                continue;
            }
            let Some(format) = Format::for_path(Path::new(entry.name())) else {
                continue;
            };
            let path = Path::new(entry.dir()).join(entry.name());
            let old_document = previously_indexed.remove(path.as_path());
            if !roots.iter().any(|root| under_root(root, &path)) {
                report.record_skip(SkipReason::Unavailable);
                continue;
            }
            if documents.len() >= MAX_DOCUMENTS
                || text_bytes >= MAX_TEXT_BYTES
                || unit_count >= MAX_TOTAL_UNITS
                || entry.size > MAX_FILE_BYTES as u64
            {
                report.record_skip(SkipReason::TooLarge);
                continue;
            }
            let (file, stamp) = match inspection::inspect(&path, approved_volumes) {
                Ok(source) => source,
                Err(err) => {
                    report_source_error(&mut report, entry.name(), old_document.is_some(), err)?;
                    continue;
                }
            };
            let (units, reused) = if let Some(doc) =
                old_document.filter(|doc| doc.format == format && doc.stamp.reusable_as(&stamp))
            {
                (Arc::clone(&doc.units), true)
            } else {
                let result = if let Some(source) = previously_skipped
                    .get(path.as_path())
                    .filter(|source| source.stamp.reusable_as(&stamp))
                {
                    Err(source.reason)
                } else {
                    let raw = match read_source(&file, &stamp) {
                        Ok(raw) => raw,
                        Err(err) => {
                            report_source_error(
                                &mut report,
                                entry.name(),
                                old_document.is_some(),
                                err,
                            )?;
                            continue;
                        }
                    };
                    drop(file);
                    extract(format, &raw, cancel)
                };
                check_cancelled(cancel)?;
                match result {
                    Ok(units) => (Arc::from(units), false),
                    Err(reason) => {
                        report.record_skip(reason);
                        let path_bytes = path.as_os_str().len();
                        if stamp.has_identity()
                            && reason.can_remember()
                            && skipped_sources.len() < MAX_REMEMBERED_SKIPS
                            && remembered_path_bytes + path_bytes <= MAX_REMEMBERED_PATH_BYTES
                        {
                            remembered_path_bytes += path_bytes;
                            skipped_sources.push(SkippedSource {
                                path,
                                stamp,
                                reason,
                            });
                        }
                        continue;
                    }
                }
            };
            let size = units.iter().map(String::len).sum::<usize>();
            if text_bytes + size > MAX_TEXT_BYTES || unit_count + units.len() > MAX_TOTAL_UNITS {
                report.record_skip(SkipReason::TooLarge);
                continue;
            }
            text_bytes += size;
            unit_count += units.len();
            if reused {
                report.reused += 1;
            } else {
                report.extracted += 1;
            }
            documents.push(Document {
                path,
                format,
                stamp,
                units,
            });
        }
        // The metadata walker can skip an unreadable descendant. Confirm that
        // omitted documents actually disappeared or became ineligible before
        // publishing a snapshot that silently loses their text.
        for doc in previously_indexed.into_values() {
            check_cancelled(cancel)?;
            let Some(parent) = doc.path.parent() else {
                continue;
            };
            match oilfind_core::validate_scan_roots(&[parent.to_path_buf()]) {
                Ok(_) => {}
                Err(
                    oilfind_core::Error::ProtectedRoot
                    | oilfind_core::Error::LinkRoot
                    | oilfind_core::Error::NotDirectory,
                ) => continue,
                Err(oilfind_core::Error::RootUnreadable(err)) if source_can_be_dropped(&err) => {
                    continue
                }
                Err(err) => {
                    return Err(format!("“{}”暂时无法读取，保留上次索引：{err}", doc.name()))
                }
            }
            match open_indexed_file(&doc.path, approved_volumes) {
                Ok(_) => return Err(format!("“{}”在扫描期间未被枚举，保留上次索引", doc.name())),
                Err(err) if source_can_be_dropped(&err) => {}
                Err(err) => return Err(retain_snapshot_message(doc.name(), &err)),
            }
        }
        #[cfg(windows)]
        verify_volumes(roots, approved_volumes)?;
        check_cancelled(cancel)?;
        Ok(Self {
            version: CACHE_VERSION,
            roots: roots.to_vec(),
            documents,
            skipped: report.skipped(),
            report: Some(report),
            skipped_sources,
        })
    }

    pub fn from_cache(data: &[u8], roots: &[PathBuf]) -> Result<Self, String> {
        if data.len() > MAX_CACHE_BYTES || roots.is_empty() {
            return Err("正文索引缓存无效".into());
        }
        let index: Self = serde_json::from_slice(data).map_err(|err| err.to_string())?;
        if index.version != CACHE_VERSION
            || index.roots != roots
            || index.len() > MAX_DOCUMENTS
            || index.skipped > oilfind_core::MAX_ENTRIES
            || index.report.is_some_and(|report| {
                report.reused.saturating_add(report.extracted) != index.len()
                    || report.skipped() != index.skipped
            })
            || index.documents.iter().any(|doc| {
                Format::for_path(&doc.path) != Some(doc.format)
                    || !roots.iter().any(|root| under_root(root, &doc.path))
                    || !valid_units(&doc.units)
            })
            || index.skipped_sources.len() > index.skipped.min(MAX_REMEMBERED_SKIPS)
            || index.skipped_sources.iter().any(|source| {
                !source.stamp.has_identity()
                    || !source.reason.can_remember()
                    || Format::for_path(&source.path).is_none()
                    || !roots.iter().any(|root| under_root(root, &source.path))
            })
            || index
                .skipped_sources
                .iter()
                .map(|source| source.path.as_os_str().len())
                .sum::<usize>()
                > MAX_REMEMBERED_PATH_BYTES
            || index
                .documents
                .iter()
                .map(|doc| doc.units.len())
                .sum::<usize>()
                > MAX_TOTAL_UNITS
            || index
                .documents
                .iter()
                .flat_map(|doc| doc.units.iter())
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

    pub fn report(&self) -> Option<RefreshReport> {
        self.report
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

#[cfg(windows)]
fn verify_volumes(roots: &[PathBuf], volumes: &BTreeMap<String, String>) -> Result<(), String> {
    if crate::platform::volume_ids(roots)? != *volumes {
        Err("正文范围所在的磁盘已变化，请重新确认范围".into())
    } else {
        Ok(())
    }
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Relaxed) {
        Err("正文索引已取消".into())
    } else {
        Ok(())
    }
}

fn source_can_be_dropped(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::IsADirectory
            | io::ErrorKind::Unsupported
            | io::ErrorKind::InvalidData
            | io::ErrorKind::FileTooLarge
    )
}

fn retain_snapshot_message(name: &str, err: &io::Error) -> String {
    format!("“{name}”暂时无法读取，保留上次索引：{err}")
}

fn report_source_error(
    report: &mut RefreshReport,
    name: &str,
    previously_indexed: bool,
    err: io::Error,
) -> Result<(), String> {
    if previously_indexed && !source_can_be_dropped(&err) {
        return Err(retain_snapshot_message(name, &err));
    }
    report.record_skip(SkipReason::from_io(&err));
    Ok(())
}

impl Document {
    pub fn name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
    }

    pub fn matches_metadata(&self, metadata: &fs::Metadata) -> bool {
        metadata.is_file() && self.stamp.matches_metadata(metadata)
    }

    pub fn matches_file(&self, file: &File) -> bool {
        if self.stamp.has_identity() {
            FileStamp::from_file(file).is_ok_and(|stamp| stamp == self.stamp)
        } else {
            file.metadata()
                .is_ok_and(|meta| self.matches_metadata(&meta))
        }
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

    pub fn pdf_match(&self, term: &str, ordinal: usize) -> Option<(usize, &str, Range<usize>)> {
        if self.format != Format::Pdf || term.is_empty() {
            return None;
        }
        let (unit, offset) = self.matches(term).nth(ordinal)?;
        Some((unit + 1, &self.units[unit], offset..offset + term.len()))
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
    units.len() <= MAX_UNITS
        && units.iter().map(String::len).sum::<usize>() <= MAX_DOC_BYTES
        && units.iter().all(|unit| !unit.contains(['\0', '\r']))
        && units.iter().any(|unit| !unit.trim().is_empty())
}

fn extract(format: Format, raw: &[u8], cancel: &AtomicBool) -> Result<Vec<String>, SkipReason> {
    let units = match format {
        Format::Pdf => pdf::units(raw, cancel)?,
        Format::Word => word_units(raw)?,
        Format::Text => {
            let text = text_file(raw).ok_or(SkipReason::ExtractionFailed)?;
            vec![normalize_text(&text)]
        }
    };
    if units.len() > MAX_UNITS || units.iter().map(String::len).sum::<usize>() > MAX_DOC_BYTES {
        Err(SkipReason::TooLarge)
    } else if units.iter().all(|unit| unit.trim().is_empty()) {
        Err(SkipReason::NoText)
    } else {
        Ok(units)
    }
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

fn word_units(bytes: &[u8]) -> Result<Vec<String>, SkipReason> {
    const WORD_TRANSITIONAL: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    const WORD_STRICT: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| SkipReason::ExtractionFailed)?;
    let xml = archive
        .by_name("word/document.xml")
        .map_err(|_| SkipReason::ExtractionFailed)?;
    if xml.size() > MAX_XML_BYTES as u64 {
        return Err(SkipReason::TooLarge);
    }
    let mut raw = Vec::new();
    xml.take(MAX_XML_BYTES as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| SkipReason::ExtractionFailed)?;
    if raw.len() > MAX_XML_BYTES {
        return Err(SkipReason::TooLarge);
    }
    let xml = text_file(&raw).ok_or(SkipReason::ExtractionFailed)?;
    let tree = roxmltree::Document::parse_with_options(
        &xml,
        roxmltree::ParsingOptions {
            nodes_limit: 300_000,
            ..Default::default()
        },
    )
    .map_err(|err| match err {
        roxmltree::Error::NodesLimitReached => SkipReason::TooLarge,
        _ => SkipReason::ExtractionFailed,
    })?;
    let body = tree
        .root_element()
        .children()
        .find(|node| {
            matches!(
                node.tag_name().namespace(),
                Some(WORD_TRANSITIONAL | WORD_STRICT)
            ) && node.has_tag_name("body")
        })
        .ok_or(SkipReason::ExtractionFailed)?;
    let namespace = body
        .tag_name()
        .namespace()
        .ok_or(SkipReason::ExtractionFailed)?;
    let mut units = Vec::<String>::new();
    let mut paragraphs = HashMap::new();
    for node in body
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().namespace() == Some(namespace))
    {
        let name = node.tag_name().name();
        if name == "p" {
            if units.len() >= MAX_UNITS {
                return Err(SkipReason::TooLarge);
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
    Ok(units)
}

pub fn read_local_file(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    #[cfg(windows)]
    let volumes = crate::platform::volume_ids(&[path.to_path_buf()]).map_err(io::Error::other)?;
    #[cfg(not(windows))]
    let volumes = BTreeMap::new();
    let (file, metadata) = inspection::open(path, &volumes)?;
    read_limited(&file, metadata.len(), limit)
}

fn read_source(file: &File, stamp: &FileStamp) -> io::Result<Vec<u8>> {
    let data = read_limited(file, stamp.bytes, MAX_FILE_BYTES)?;
    if stamp.bytes != data.len() as u64 || *stamp != FileStamp::from_file(file)? {
        return Err(io::Error::other("读取正文时文件发生变化"));
    }
    Ok(data)
}

pub fn open_indexed_file(
    path: &Path,
    approved_volumes: &BTreeMap<String, String>,
) -> io::Result<File> {
    inspection::open(path, approved_volumes).map(|(file, _)| file)
}

fn read_limited(file: &File, size: u64, limit: usize) -> io::Result<Vec<u8>> {
    if size > limit as u64 {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "文件超出大小限制",
        ));
    }
    let mut data = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut data)?;
    if data.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "文件超出大小限制",
        ));
    }
    Ok(data)
}

fn under_root(root: &Path, path: &Path) -> bool {
    let safe = path.is_absolute()
        && path.to_str().is_some()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir));
    safe && path.starts_with(root)
}
