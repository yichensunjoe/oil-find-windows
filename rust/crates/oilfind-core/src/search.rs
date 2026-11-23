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
use std::borrow::Cow;
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
struct Ranked<'a> {
    entry: Cow<'a, Entry>,
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
        self.search_by(query, kind, limit, |_, _| Ordering::Equal)
    }

    /// Orders all matches with the supplied comparison before applying the limit.
    ///
    /// A comparison of `Less` puts the first entry earlier in the results.
    /// Equal entries use the normal relevance and path ordering.
    pub fn search_by(
        &self,
        query: &str,
        kind: Option<&str>,
        limit: usize,
        order: impl Fn(&Entry, &Entry) -> Ordering,
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

        let compare = |a: &Ranked<'_>, b: &Ranked<'_>| {
            order(&a.entry, &b.entry)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| a.entry.lower_path().cmp(&b.entry.lower_path()))
        };
        let mut heap = Vec::with_capacity(limit.min(self.entries.len()));
        let mut total = 0usize;
        for stored in &self.entries {
            // Entries built by an external caller may not have their derived
            // fields populated yet.
            let entry = if stored.lower_name().is_empty() {
                let mut copy = stored.clone();
                copy.prepare();
                Cow::Owned(copy)
            } else {
                Cow::Borrowed(stored)
            };
            if let Some(category) = category.as_ref() {
                if !category(&entry) {
                    continue;
                }
            }
            if !compiled.matches(&entry) {
                continue;
            }
            total += 1;
            let score = compiled
                .rank_terms
                .iter()
                .map(|term| score_of(&entry, term))
                .sum();
            let candidate = Ranked { entry, score };
            if heap.len() < limit {
                heap_push(&mut heap, candidate, &compare);
            } else if compare(&candidate, &heap[0]).is_lt() {
                heap_replace_root(&mut heap, candidate, &compare);
            }
        }

        heap.sort_by(compare);
        let rows = heap
            .into_iter()
            .map(|item| item.entry.into_owned())
            .collect();
        Ok((rows, total))
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

/// Inserts into a heap whose root is the worst retained candidate.
fn heap_push<'a>(
    heap: &mut Vec<Ranked<'a>>,
    item: Ranked<'a>,
    compare: &impl Fn(&Ranked<'a>, &Ranked<'a>) -> Ordering,
) {
    heap.push(item);
    let mut child = heap.len() - 1;
    while child > 0 {
        let parent = (child - 1) / 2;
        if !compare(&heap[parent], &heap[child]).is_lt() {
            break;
        }
        heap.swap(parent, child);
        child = parent;
    }
}

/// Replaces the worst retained candidate and sifts the new one down.
fn heap_replace_root<'a>(
    heap: &mut [Ranked<'a>],
    item: Ranked<'a>,
    compare: &impl Fn(&Ranked<'a>, &Ranked<'a>) -> Ordering,
) {
    heap[0] = item;
    let mut parent = 0;
    loop {
        let mut child = parent * 2 + 1;
        if child >= heap.len() {
            break;
        }
        if child + 1 < heap.len() && compare(&heap[child], &heap[child + 1]).is_lt() {
            child += 1;
        }
        if !compare(&heap[parent], &heap[child]).is_lt() {
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
                    Box::new(move |entry: &Entry| regex.is_match(entry.name())),
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
                        !entry.is_dir && extensions.iter().any(|x| extension_of(entry.name()) == *x)
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
                        !entry.is_dir && extensions.iter().any(|x| extension_of(entry.name()) == *x)
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
    let today = time::local_day_start(now);
    let next_day = |start| time::offset_local_day(start, 1).ok_or(Error::InvalidDate);
    let (start, end) = match value {
        "today" => (today, next_day(today)?),
        "yesterday" => (
            time::offset_local_day(today, -1).ok_or(Error::InvalidDate)?,
            today,
        ),
        "week" => (
            time::offset_local_day(today, -6).ok_or(Error::InvalidDate)?,
            next_day(today)?,
        ),
        "month" => (
            time::local_month_start(now),
            time::next_local_month_start(now).ok_or(Error::InvalidDate)?,
        ),
        _ => {
            if let Some((low, high)) = value.split_once("..") {
                let start = time::parse_local_date(low).ok_or(Error::InvalidDate)?;
                let end = time::parse_local_date(high).ok_or(Error::InvalidDateRange)?;
                if end < start {
                    return Err(Error::InvalidDateRange);
                }
                (start, next_day(end).map_err(|_| Error::InvalidDateRange)?)
            } else {
                let start = time::parse_local_date(value).ok_or(Error::InvalidDate)?;
                (start, next_day(start)?)
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

    #[cfg(unix)]
    #[test]
    fn date_predicates_follow_local_calendar_across_dst() {
        const CHILD: &str = "OILFIND_DST_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            for (case, zone) in [
                ("fall", "America/Los_Angeles"),
                ("midnight", "America/Sao_Paulo"),
            ] {
                let output =
                    std::process::Command::new(std::env::current_exe().expect("test executable"))
                        .args([
                            "--exact",
                            "search::tests::date_predicates_follow_local_calendar_across_dst",
                        ])
                        .env(CHILD, case)
                        .env("TZ", zone)
                        .output()
                        .expect("run isolated timezone test");
                assert!(
                    output.status.success(),
                    "timezone test {zone} failed:\n{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        }

        use chrono::{Local, TimeZone};
        use std::path::PathBuf;
        if std::env::var(CHILD).as_deref() == Ok("midnight") {
            let noon = Local
                .with_ymd_and_hms(2018, 11, 4, 12, 0, 0)
                .single()
                .unwrap()
                .timestamp();
            let first = time::parse_local_date("2018-11-04").expect("first valid instant");
            let local = Local.timestamp_opt(first, 0).single().unwrap();
            assert_eq!(local.format("%H:%M").to_string(), "01:00");
            let entry = Entry::new(PathBuf::from("/fixture.txt"), String::new(), 0, noon, false);
            assert!(date_predicate("2018-11-04", noon).unwrap()(&entry));
            return;
        }
        let stamp = |month, day| {
            Local
                .with_ymd_and_hms(2026, month, day, 12, 0, 0)
                .single()
                .expect("local noon")
                .timestamp()
        };
        let entry = |modified| {
            Entry::new(
                PathBuf::from("/fixture.txt"),
                String::new(),
                0,
                modified,
                false,
            )
        };

        let fall = entry(stamp(11, 1));
        assert!(date_predicate("2026-11-01", stamp(11, 2)).unwrap()(&fall));
        assert!(date_predicate("2026-11-01..2026-11-01", stamp(11, 2)).unwrap()(&fall));
        assert!(date_predicate("today", stamp(11, 1)).unwrap()(&fall));

        let yesterday = date_predicate("yesterday", stamp(3, 9)).unwrap();
        assert!(yesterday(&entry(stamp(3, 8))));
        assert!(!yesterday(&entry(stamp(3, 7))));
        let week = date_predicate("week", stamp(3, 9)).unwrap();
        assert!(week(&entry(stamp(3, 3))));
        assert!(!week(&entry(stamp(3, 2))));
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
