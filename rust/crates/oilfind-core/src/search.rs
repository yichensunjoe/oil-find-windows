//! Query compilation and ranked matching.
//!
//! The grammar mirrors the Go implementation exactly: whitespace is AND, `|` is
//! OR (inside one group only), `!` negates, quotes keep spaces, and `key:value`
//! filters are recognised for `ext`, `kind`, `file`, `folder`, `path`, `size`,
//! `dm` and `regex`.

use crate::entry::{extension_of, Entry, Index};
use crate::error::Error;
use crate::time;
use regex::Regex;
use std::cmp::Ordering;

/// Maximum accepted query length, in bytes.
const MAX_QUERY_BYTES: usize = 4096;

/// Maximum number of conditions in one query.
const MAX_TERMS: usize = 128;

/// Default and maximum result sizes.
const DEFAULT_LIMIT: usize = 2000;
const MAX_LIMIT: usize = 10_000;

type Predicate = Box<dyn Fn(&Entry) -> bool + Send + Sync>;

/// A compiled query: one OR group list plus the terms that contribute to rank.
struct Compiled {
    groups: Vec<Vec<Predicate>>,
    rank_terms: Vec<String>,
}

impl Compiled {
    fn matches(&self, entry: &Entry) -> bool {
        if self.groups.is_empty() {
            return true;
        }
        self.groups
            .iter()
            .any(|group| group.iter().all(|predicate| predicate(entry)))
    }
}

/// A retained candidate together with its relevance score.
struct Ranked {
    entry: Entry,
    score: i64,
}

impl Index {
    /// Searches the index.
    ///
    /// Returns the best `limit` entries together with the total number of
    /// matches, which is independent of `limit`. `limit == 0` means the default
    /// of 2000; the value is capped at 10000.
    pub fn search(
        &self,
        query: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<Entry>, usize), Error> {
        let now = time::now_secs();
        let category = match kind {
            Some(kind) if !kind.is_empty() => {
                let term = if kind == "file" {
                    "file:".to_string()
                } else {
                    format!("kind:{kind}")
                };
                Some(term_predicate(&term, now)?.0)
            }
            _ => None,
        };
        if query.len() > MAX_QUERY_BYTES {
            return Err(Error::QueryTooLong);
        }
        let compiled = compile(query, now)?;
        let limit = match limit {
            0 => DEFAULT_LIMIT,
            limit => limit.min(MAX_LIMIT),
        };

        let mut heap: Vec<Ranked> = Vec::with_capacity(limit.min(self.entries.len()));
        let mut total = 0usize;
        for stored in &self.entries {
            // Entries built by an external caller may not have their derived
            // fields populated yet.
            let prepared;
            let entry = if stored.lower_name().is_empty() {
                prepared = {
                    let mut copy = stored.clone();
                    copy.prepare();
                    copy
                };
                &prepared
            } else {
                stored
            };
            if let Some(category) = category.as_ref() {
                if !category(entry) {
                    continue;
                }
            }
            if !compiled.matches(entry) {
                continue;
            }
            total += 1;
            let score = compiled
                .rank_terms
                .iter()
                .map(|term| score_of(entry, term))
                .sum();
            if heap.len() < limit {
                heap_push(
                    &mut heap,
                    Ranked {
                        entry: entry.clone(),
                        score,
                    },
                );
            } else {
                // Compare before cloning: most matches are dropped here.
                let root = &heap[0];
                let keeps = score > root.score
                    || (score == root.score && entry.lower_path() < root.entry.lower_path());
                if keeps {
                    heap_replace_root(
                        &mut heap,
                        Ranked {
                            entry: entry.clone(),
                            score,
                        },
                    );
                }
            }
        }

        heap.sort_by(|a, b| {
            if better(a, b) {
                Ordering::Less
            } else if better(b, a) {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        });
        Ok((heap.into_iter().map(|item| item.entry).collect(), total))
    }
}

/// Scores one ranking term: exact name, name without extension, prefix,
/// substring, then pinyin.
fn score_of(entry: &Entry, term: &str) -> i64 {
    let name = entry.lower_name();
    if name == term {
        return 100;
    }
    let stem = match name.rfind('.') {
        Some(index) if index > 0 => &name[..index],
        _ => name,
    };
    if stem == term {
        return 90;
    }
    if name.starts_with(term) {
        return 60;
    }
    if name.contains(term) {
        return 30;
    }
    if entry.full_pinyin() == term || entry.initials() == term {
        return 25;
    }
    10
}

/// True when `a` should be shown before `b`. Ties break on the lowercased path
/// so results are deterministic across runs.
fn better(a: &Ranked, b: &Ranked) -> bool {
    if a.score != b.score {
        a.score > b.score
    } else {
        a.entry.lower_path() < b.entry.lower_path()
    }
}

/// Inserts into a heap whose root is the worst retained candidate.
fn heap_push(heap: &mut Vec<Ranked>, item: Ranked) {
    heap.push(item);
    let mut child = heap.len() - 1;
    while child > 0 {
        let parent = (child - 1) / 2;
        if !better(&heap[parent], &heap[child]) {
            break;
        }
        heap.swap(parent, child);
        child = parent;
    }
}

/// Replaces the worst retained candidate and sifts the new one down.
fn heap_replace_root(heap: &mut [Ranked], item: Ranked) {
    heap[0] = item;
    let mut parent = 0;
    loop {
        let mut child = parent * 2 + 1;
        if child >= heap.len() {
            break;
        }
        if child + 1 < heap.len() && better(&heap[child], &heap[child + 1]) {
            child += 1;
        }
        if !better(&heap[parent], &heap[child]) {
            break;
        }
        heap.swap(parent, child);
        parent = child;
    }
}

/// Splits a query into tokens, keeping quoted text intact and treating a bare
/// `|` as an OR operator (but not inside a regular expression).
fn tokenize(text: &str) -> Result<Vec<String>, Error> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;

    for ch in text.chars() {
        if let Some(opening) = quote {
            if ch == opening {
                quote = None;
                // An empty quoted token is a user error, not "match everything".
                if current.is_empty() {
                    tokens.push(String::new());
                }
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() {
            flush(&mut tokens, &mut current);
            continue;
        }
        // A bar inside a regex stays part of the expression; surround OR
        // operators with spaces when combining a regex with other groups.
        if ch == '|' && !current.starts_with("regex:") && !current.starts_with("!regex:") {
            flush(&mut tokens, &mut current);
            tokens.push("|".to_string());
            continue;
        }
        current.push(ch);
    }
    if quote.is_some() {
        return Err(Error::UnclosedQuote);
    }
    flush(&mut tokens, &mut current);
    Ok(tokens)
}

fn flush(tokens: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
}

/// Turns a query string into OR groups of ANDed predicates.
fn compile(text: &str, now: i64) -> Result<Compiled, Error> {
    let tokens = tokenize(text)?;
    if tokens.len() > MAX_TERMS {
        return Err(Error::TooManyTerms);
    }
    let mut compiled = Compiled {
        groups: Vec::new(),
        rank_terms: Vec::new(),
    };
    if tokens.is_empty() {
        return Ok(compiled);
    }

    let mut group: Vec<Predicate> = Vec::new();
    for token in tokens {
        if token == "|" {
            if group.is_empty() {
                return Err(Error::EmptyOr);
            }
            compiled.groups.push(std::mem::take(&mut group));
            continue;
        }
        let (negated, body) = match token.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, token.as_str()),
        };
        if negated && body.is_empty() {
            return Err(Error::EmptyTerm);
        }
        let (predicate, rank) = term_predicate(body, now)?;
        if negated {
            group.push(Box::new(move |entry: &Entry| !predicate(entry)));
        } else {
            if !rank.is_empty() {
                compiled.rank_terms.push(rank);
            }
            group.push(predicate);
        }
    }
    if group.is_empty() {
        return Err(Error::EmptyOr);
    }
    compiled.groups.push(group);
    Ok(compiled)
}

/// Builds one predicate plus, for plain terms, the string used for ranking.
fn term_predicate(token: &str, now: i64) -> Result<(Predicate, String), Error> {
    let low = token.to_lowercase();
    if let Some((key, value)) = low.split_once(':') {
        match key {
            "regex" => {
                // Keep the caller's escaping and case; only add insensitive mode.
                let raw = &token["regex:".len()..];
                if raw.is_empty() {
                    return Err(Error::InvalidRegex("正则表达式不能为空".to_string()));
                }
                let pattern = format!("(?i){raw}");
                let regex =
                    Regex::new(&pattern).map_err(|err| Error::InvalidRegex(err.to_string()))?;
                return Ok((
                    Box::new(move |entry: &Entry| regex.is_match(&entry.name)),
                    String::new(),
                ));
            }
            "ext" => {
                if value.is_empty() {
                    return Err(Error::InvalidExtension);
                }
                let mut extensions: Vec<String> = Vec::new();
                for part in value.split(';') {
                    let part = part.strip_prefix('.').unwrap_or(part);
                    if part.is_empty() {
                        return Err(Error::InvalidExtension);
                    }
                    extensions.push(format!(".{part}"));
                }
                return Ok((
                    Box::new(move |entry: &Entry| {
                        !entry.is_dir && extensions.iter().any(|x| extension_of(&entry.name) == *x)
                    }),
                    String::new(),
                ));
            }
            "kind" => {
                if value == "folder" {
                    return Ok((Box::new(|entry: &Entry| entry.is_dir), String::new()));
                }
                let extensions = kind_extensions(value)
                    .ok_or_else(|| Error::UnsupportedKind(value.to_string()))?;
                return Ok((
                    Box::new(move |entry: &Entry| {
                        !entry.is_dir && extensions.iter().any(|x| extension_of(&entry.name) == *x)
                    }),
                    String::new(),
                ));
            }
            "file" | "folder" => {
                let wants_directory = key == "folder";
                let needle = value.to_string();
                let rank = needle.clone();
                return Ok((
                    Box::new(move |entry: &Entry| {
                        entry.is_dir == wants_directory
                            && (needle.is_empty() || name_contains(entry, &needle))
                    }),
                    rank,
                ));
            }
            "path" => {
                if value.is_empty() {
                    return Err(Error::EmptyPath);
                }
                let needle = value.replace('\\', "/");
                return Ok((
                    Box::new(move |entry: &Entry| {
                        entry.lower_path().replace('\\', "/").contains(&needle)
                    }),
                    String::new(),
                ));
            }
            "size" => return Ok((size_predicate(value)?, String::new())),
            "dm" => return Ok((date_predicate(value, now)?, String::new())),
            _ => {}
        }
    }

    if low.contains('*') || low.contains('?') {
        let pattern: Vec<char> = low.chars().collect();
        return Ok((
            Box::new(move |entry: &Entry| {
                let name: Vec<char> = entry.lower_name().chars().collect();
                glob_match(&pattern, &name)
            }),
            String::new(),
        ));
    }
    if low.is_empty() {
        return Err(Error::EmptyTerm);
    }
    let rank = low.clone();
    Ok((
        Box::new(move |entry: &Entry| {
            name_contains(entry, &low) || entry.lower_path().contains(&low)
        }),
        rank,
    ))
}

/// Matches a name against its own text, its full pinyin and its initials.
fn name_contains(entry: &Entry, needle: &str) -> bool {
    entry.lower_name().contains(needle)
        || entry.full_pinyin().contains(needle)
        || entry.initials().contains(needle)
}

/// `*` matches any run of characters, `?` matches exactly one, everything else
/// (including `[`) is literal. The pattern must cover the whole name.
fn glob_match(pattern: &[char], text: &[char]) -> bool {
    let (mut pattern_index, mut text_index) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut resume_at = 0usize;
    while text_index < text.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == text[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star = Some(pattern_index);
            pattern_index += 1;
            resume_at = text_index;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            resume_at += 1;
            text_index = resume_at;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

/// Parses a byte size such as `10mb` or `1.5gb`.
fn parse_size(text: &str) -> Result<u64, Error> {
    const UNITS: [(&str, f64); 5] = [
        ("tb", 1_099_511_627_776.0),
        ("gb", 1_073_741_824.0),
        ("mb", 1_048_576.0),
        ("kb", 1_024.0),
        ("b", 1.0),
    ];
    let mut factor = 1.0f64;
    let mut body = text;
    for (suffix, unit) in UNITS {
        if let Some(rest) = text.strip_suffix(suffix) {
            body = rest;
            factor = unit;
            break;
        }
    }
    let value: f64 = body.parse().map_err(|_| Error::InvalidSize)?;
    if !value.is_finite() || value < 0.0 || value >= (i64::MAX as f64) / factor {
        return Err(Error::InvalidSize);
    }
    Ok((value * factor) as u64)
}

fn size_predicate(value: &str) -> Result<Predicate, Error> {
    if let Some((low, high)) = value.split_once("..") {
        let low = parse_size(low)?;
        let high = parse_size(high).map_err(|_| Error::InvalidSizeRange)?;
        if high < low {
            return Err(Error::InvalidSizeRange);
        }
        return Ok(Box::new(move |entry: &Entry| {
            !entry.is_dir && entry.size >= low && entry.size <= high
        }));
    }
    let mut operator = "=";
    let mut body = value;
    for candidate in [">=", "<=", ">", "<", "="] {
        if let Some(rest) = value.strip_prefix(candidate) {
            operator = candidate;
            body = rest;
            break;
        }
    }
    let limit = parse_size(body)?;
    let operator = operator.to_string();
    Ok(Box::new(move |entry: &Entry| {
        if entry.is_dir {
            return false;
        }
        match operator.as_str() {
            ">" => entry.size > limit,
            ">=" => entry.size >= limit,
            "<" => entry.size < limit,
            "<=" => entry.size <= limit,
            _ => entry.size == limit,
        }
    }))
}

fn date_predicate(value: &str, now: i64) -> Result<Predicate, Error> {
    const DAY: i64 = 86_400;
    let today = time::local_day_start(now);
    let (start, end) = match value {
        "today" => (today, time::local_day_start(today + DAY)),
        "yesterday" => (time::local_day_start(today - DAY), today),
        "week" => (
            time::local_day_start(today - 6 * DAY),
            time::local_day_start(today + DAY),
        ),
        "month" => {
            let start = time::local_month_start(now);
            (start, time::local_month_start(start + 32 * DAY))
        }
        _ => {
            if let Some((low, high)) = value.split_once("..") {
                let start = time::parse_local_date(low).ok_or(Error::InvalidDate)?;
                let end = time::parse_local_date(high).ok_or(Error::InvalidDateRange)?;
                if end < start {
                    return Err(Error::InvalidDateRange);
                }
                (start, time::local_day_start(end + DAY))
            } else {
                let start = time::parse_local_date(value).ok_or(Error::InvalidDate)?;
                (start, time::local_day_start(start + DAY))
            }
        }
    };
    Ok(Box::new(move |entry: &Entry| {
        entry.modified >= start && entry.modified < end
    }))
}

/// Extension lists per UI category. `doc` is the widest one on purpose: it is
/// the category users pick for "everything I can open and read".
fn kind_extensions(kind: &str) -> Option<&'static [&'static str]> {
    const IMAGE: &[&str] = &[
        ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".tif", ".tiff", ".svg", ".heic",
    ];
    const DOC: &[&str] = &[
        ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".txt", ".md", ".rtf", ".csv",
        ".odt", ".ods", ".odp", ".wps", ".et", ".dps", ".ofd",
    ];
    const VIDEO: &[&str] = &[".mp4", ".mov", ".mkv", ".avi", ".wmv", ".webm", ".m4v"];
    const AUDIO: &[&str] = &[".mp3", ".wav", ".flac", ".aac", ".m4a", ".ogg", ".wma"];
    const CODE: &[&str] = &[
        ".go", ".py", ".js", ".ts", ".tsx", ".jsx", ".c", ".cpp", ".h", ".hpp", ".cs", ".java",
        ".rs", ".swift", ".html", ".css", ".json", ".yaml", ".yml", ".xml", ".sql", ".sh", ".ps1",
    ];
    const ARCHIVE: &[&str] = &[".zip", ".7z", ".rar", ".gz", ".tar", ".bz2", ".xz"];
    const APP: &[&str] = &[".exe", ".msi", ".msix", ".com"];

    match kind {
        "image" => Some(IMAGE),
        "doc" => Some(DOC),
        "video" => Some(VIDEO),
        "audio" => Some(AUDIO),
        "code" => Some(CODE),
        "archive" => Some(ARCHIVE),
        "app" => Some(APP),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_whole_names() {
        let pattern: Vec<char> = "*.png".chars().collect();
        assert!(glob_match(&pattern, &"a.png".chars().collect::<Vec<_>>()));
        assert!(!glob_match(
            &pattern,
            &"a.png.txt".chars().collect::<Vec<_>>()
        ));
        let bracket: Vec<char> = "[a].txt".chars().collect();
        assert!(glob_match(&bracket, &"[a].txt".chars().collect::<Vec<_>>()));
        assert!(!glob_match(&bracket, &"a.txt".chars().collect::<Vec<_>>()));
    }

    #[test]
    fn tokenizer_keeps_quoted_spaces_and_regex_bars() {
        assert_eq!(
            tokenize("\"a b\" c").unwrap(),
            vec!["a b".to_string(), "c".to_string()]
        );
        assert_eq!(
            tokenize("regex:a|b").unwrap(),
            vec!["regex:a|b".to_string()]
        );
        assert_eq!(
            tokenize("a | b").unwrap(),
            vec!["a".to_string(), "|".to_string(), "b".to_string()]
        );
        assert!(tokenize("\"a").is_err());
    }
}
