//! Extract text while preserving explicitly positioned lines and form boundaries.
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use lopdf::{content::Content, Dictionary, Document, Encoding, Object, ObjectId};

use super::{normalize_text, MAX_DOC_BYTES};

const STREAM_LIMIT: usize = 8 * 1024 * 1024;
const MAX_PAGES: usize = 4096;
const MAX_FORM_DEPTH: usize = 8;

pub(super) fn units(bytes: &[u8], cancel: &AtomicBool) -> Option<Vec<String>> {
    let options = lopdf::LoadOptions::with_max_decompressed_size(STREAM_LIMIT);
    let pdf = Document::load_mem_with_options(bytes, options).ok()?;
    if pdf.is_encrypted() {
        return None;
    }
    let mut total = 0;
    let mut units = Vec::new();
    for page in pdf.page_iter() {
        if cancel.load(Relaxed) || units.len() >= MAX_PAGES {
            return None;
        }
        // An undecodable page must not shift the numbering of later hits.
        let text = page_text(&pdf, page).unwrap_or_default();
        total += text.len();
        if total > MAX_DOC_BYTES {
            return None;
        }
        units.push(text);
    }
    Some(units)
}

fn page_text(pdf: &Document, page: ObjectId) -> Option<String> {
    let (direct, references) = pdf.get_page_resources(page).ok()?;
    let resources = direct.or_else(|| {
        references
            .into_iter()
            .find_map(|id| pdf.get_dictionary(id).ok())
    });
    let bytes = pdf.get_page_content_with_limit(page, STREAM_LIMIT).ok()?;
    let mut output = PageText {
        pdf,
        remaining: STREAM_LIMIT,
        text: String::new(),
    };
    output.capture(&bytes, resources, 0)?;
    Some(normalize_text(&output.text))
}

struct PageText<'a> {
    pdf: &'a Document,
    remaining: usize,
    text: String,
}

impl<'a> PageText<'a> {
    fn capture(
        &mut self,
        bytes: &[u8],
        resources: Option<&'a Dictionary>,
        depth: usize,
    ) -> Option<()> {
        self.remaining = self.remaining.checked_sub(bytes.len())?;
        let content = Content::decode(bytes).ok()?;
        let fonts = font_encodings(self.pdf, resources);
        let mut font = None;
        let mut baseline = Some(0.0);
        for operation in &content.operations {
            let args = &operation.operands;
            let operator = operation.operator.as_str();
            match operator {
                "BT" | "ET" | "T*" => {
                    self.break_line();
                    baseline = (operator == "BT").then_some(0.0);
                }
                "Td" | "TD" => {
                    if let Some(dy) = args.get(1).and_then(|value| value.as_float().ok()) {
                        if dy != 0.0 {
                            self.break_line();
                        }
                        baseline = baseline.map(|y| y + dy);
                    }
                }
                "Tm" => {
                    if let Some(y) = args.get(5).and_then(|value| value.as_float().ok()) {
                        if baseline.is_some_and(|previous| previous != y) {
                            self.break_line();
                        }
                        baseline = Some(y);
                    }
                }
                "Tf" => {
                    font = args
                        .first()
                        .and_then(|value| value.as_name().ok())
                        .and_then(|name| fonts.get(name));
                }
                "Tj" | "'" | "\"" => {
                    if operator != "Tj" {
                        self.break_line();
                        baseline = None;
                    }
                    let index = if operator == "\"" { 2 } else { 0 };
                    if let Some(value) = args.get(index) {
                        self.append(value, font);
                    }
                }
                "TJ" => {
                    if let Some(values) = args.first().and_then(|value| value.as_array().ok()) {
                        for value in values {
                            if value.as_float().is_ok_and(|space| space < -100.0) {
                                self.space();
                            } else if matches!(value, Object::String(..)) {
                                self.append(value, font);
                            }
                        }
                    }
                }
                "Do" if depth < MAX_FORM_DEPTH => {
                    if let Some(name) = args.first().and_then(|value| value.as_name().ok()) {
                        self.capture_form(name, resources, depth);
                    }
                }
                _ => {}
            }
            if self.text.len() > MAX_DOC_BYTES {
                return None;
            }
        }
        Some(())
    }

    fn capture_form(&mut self, name: &[u8], resources: Option<&'a Dictionary>, depth: usize) {
        let Some(form) = resources
            .and_then(|dict| dict.get_deref(b"XObject", self.pdf).ok())
            .and_then(|value| value.as_dict().ok())
            .and_then(|dict| dict.get_deref(name, self.pdf).ok())
            .and_then(|value| value.as_stream().ok())
            .filter(|stream| {
                stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Form")
            })
        else {
            return;
        };
        let bytes = if form.dict.has(b"Filter") {
            let Ok(bytes) = form.decompressed_content_with_limit(self.remaining) else {
                return;
            };
            Cow::Owned(bytes)
        } else {
            Cow::Borrowed(form.content.as_slice())
        };
        let local = form
            .dict
            .get_deref(b"Resources", self.pdf)
            .ok()
            .and_then(|value| value.as_dict().ok())
            .or(resources);
        self.break_line();
        let _ = self.capture(&bytes, local, depth + 1);
        self.break_line();
    }

    fn append(&mut self, value: &Object, font: Option<&Encoding<'_>>) {
        let decoded = font
            .zip(value.as_str().ok())
            .and_then(|(font, bytes)| Document::decode_text(font, bytes).ok());
        if let Some(text) = decoded {
            self.text.push_str(&text);
        } else {
            self.space();
        }
    }

    fn break_line(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
    }

    fn space(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with(char::is_whitespace) {
            self.text.push(' ');
        }
    }
}

fn font_encodings<'a>(
    pdf: &'a Document,
    resources: Option<&'a Dictionary>,
) -> BTreeMap<Vec<u8>, Encoding<'a>> {
    let Some(fonts) = resources
        .and_then(|dict| dict.get_deref(b"Font", pdf).ok())
        .and_then(|value| value.as_dict().ok())
    else {
        return BTreeMap::new();
    };
    fonts
        .iter()
        .filter_map(|(name, _)| {
            let font = fonts.get_deref(name, pdf).ok()?.as_dict().ok()?;
            let encoding = font.get_font_encoding_with_limit(pdf, STREAM_LIMIT).ok()?;
            Some((name.clone(), encoding))
        })
        .collect()
}
