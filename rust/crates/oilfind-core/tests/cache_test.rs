//! Cache persistence: round trip, atomicity, cancellation and corruption.

mod common;

use common::{json_escape, search_fixture, sorted_expected, TempDir};
use oilfind_core::{Entry, Error, Index, MAX_CACHE_BYTES};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn sample_index(root: &Path) -> Index {
    Index {
        entries: vec![
            Entry::new(root.join("a.txt"), "a.txt".to_string(), 3, 100, false),
            Entry::new(
                root.join("docs/会议纪要.docx"),
                "会议纪要.docx".to_string(),
                7,
                200,
                false,
            ),
            Entry::new(root.join("docs"), "docs".to_string(), 0, 300, true),
        ],
        roots: vec![root.to_path_buf()],
        built_at: 1_700_000_000,
    }
}

fn cache_path(dir: &TempDir) -> PathBuf {
    dir.join("cache/index.json")
}

#[test]
fn cache_round_trip_rebuilds_search_metadata() {
    let (_dir, index) = search_fixture();
    let cache_dir = TempDir::new("cache-roundtrip");
    let path = cache_path(&cache_dir);

    index.save(&path).expect("save");
    let loaded = Index::load(&path).expect("load");

    assert_eq!(loaded.entries, index.entries);
    assert_eq!(loaded.roots, index.roots);
    assert_eq!(loaded.built_at, index.built_at);

    // Derived fields are rebuilt, so pinyin and filters still work.
    let (results, total) = loaded.search("HUIYI", None, 100).expect("search");
    assert_eq!(total, 1);
    assert_eq!(results[0].name(), "会议纪要.docx");
    assert_eq!(loaded.search("hyjy", None, 100).expect("search").1, 1);
    let (results, total) = loaded
        .search("report !archive ext:pdf", None, 100)
        .expect("search");
    assert_eq!(total, 1);
    assert_eq!(results[0].name(), "Report Final.PDF");

    // No temporary file may survive a successful save.
    let names: Vec<String> = fs::read_dir(path.parent().expect("parent"))
        .expect("read cache directory")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, vec!["index.json".to_string()]);
}

#[test]
fn cache_is_written_in_the_binary_format() {
    let dir = TempDir::new("cache-shape");
    let path = cache_path(&dir);
    fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
    // Many entries under one directory: this is the case the directory table
    // exists for, and the case a real index is made of.
    let index = Index {
        entries: (0..400)
            .map(|i| {
                let name = format!("会议纪要-{i:04}.docx");
                Entry::new(dir.join(&name), name, i as u64, 100 + i, false)
            })
            .collect(),
        roots: vec![dir.path().to_path_buf()],
        built_at: 1_700_000_000,
    };
    index.save(&path).expect("save");

    let bytes = fs::read(&path).expect("read cache");
    assert_eq!(&bytes[..8], b"OILF1\0\0\0", "missing binary magic");
    // The format version follows the magic as a single-byte varint.
    assert_eq!(bytes[8], 1, "unexpected cache version");
    assert!(
        serde_json::from_slice::<serde_json::Value>(&bytes).is_err(),
        "cache is still JSON"
    );
    // Each path prefix is stored once instead of once per entry, so the file
    // is far below half of the JSON document it replaced.
    let json = serde_json::to_vec(&index).expect("serialize json");
    assert!(
        bytes.len() * 2 < json.len(),
        "binary cache ({} bytes) is not smaller than JSON ({} bytes)",
        bytes.len(),
        json.len()
    );

    let loaded = Index::load(&path).expect("load");
    assert_eq!(loaded.entries, index.entries);
    assert_eq!(loaded.roots, index.roots);
    assert_eq!(loaded.built_at, index.built_at);
}

#[test]
fn cache_rewrites_a_legacy_json_document_as_binary() {
    let dir = TempDir::new("cache-upgrade");
    let path = cache_path(&dir);
    fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
    let document = format!(
        r#"{{"version":1,"roots":["{root}"],"builtAt":"2023-11-15T06:13:20Z",
"entries":[{{"path":"{root}/会议纪要.docx","name":"会议纪要.docx","size":7,"modified":200,"isDir":false}}]}}"#,
        root = json_escape(&dir.path().to_string_lossy())
    );
    fs::write(&path, document).expect("write cache");

    let loaded = Index::load(&path).expect("load legacy cache");
    loaded.save(&path).expect("save");

    let bytes = fs::read(&path).expect("read cache");
    assert_eq!(
        &bytes[..8],
        b"OILF1\0\0\0",
        "upgrade did not rewrite as binary"
    );
    assert_eq!(Index::load(&path).expect("load").entries, loaded.entries);
}

/// Appends a bincode-style LEB128 varint.
fn varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Header of a binary cache: magic, version, roots, built_at, then the lengths
/// of the directory and entry tables.
fn binary_header(directories: u64, entries: u64) -> Vec<u8> {
    let mut data = b"OILF1\0\0\0".to_vec();
    varint(1, &mut data);
    varint(0, &mut data);
    varint(0, &mut data);
    varint(directories, &mut data);
    varint(entries, &mut data);
    data
}

#[test]
fn cache_rejects_a_binary_document_with_an_incompatible_version() {
    let dir = TempDir::new("cache-binary-version");
    let path = cache_path(&dir);
    sample_index(dir.path()).save(&path).expect("save");
    let mut bytes = fs::read(&path).expect("read cache");
    bytes[8] = 2;

    fs::write(&path, &bytes).expect("write cache");
    let error = Index::load(&path).expect_err("future version must be rejected");
    assert!(matches!(error, Error::CacheCorrupt(_)), "got: {error}");
}

#[test]
fn cache_rejects_trailing_binary_data() {
    let dir = TempDir::new("cache-binary-trailing");
    let path = cache_path(&dir);
    sample_index(dir.path()).save(&path).expect("save");
    let mut bytes = fs::read(&path).expect("read cache");
    bytes.extend_from_slice(b"leftover");

    fs::write(&path, &bytes).expect("write cache");
    let error = Index::load(&path).expect_err("trailing bytes must be rejected");
    assert!(matches!(error, Error::CacheCorrupt(_)), "got: {error}");
}

#[test]
fn cache_rejects_a_declared_entry_count_over_the_limit() {
    let dir = TempDir::new("cache-binary-count");
    let path = cache_path(&dir);
    // The count is checked before the vector is allocated, so this file is
    // rejected without ever trying to reserve room for the entries.
    let bytes = binary_header(0, oilfind_core::MAX_ENTRIES as u64 + 1);
    fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
    fs::write(&path, &bytes).expect("write cache");

    let error = Index::load(&path).expect_err("oversized entry count must be rejected");
    assert!(matches!(error, Error::CacheCorrupt(_)), "got: {error}");
}

#[test]
fn cache_rejects_an_entry_pointing_outside_the_directory_table() {
    let dir = TempDir::new("cache-binary-index");
    let path = cache_path(&dir);
    let mut bytes = binary_header(0, 1);
    varint(3, &mut bytes); // directory index, but the table is empty
    varint(5, &mut bytes);
    bytes.extend_from_slice(b"a.txt");
    varint(1, &mut bytes); // size
    varint(0, &mut bytes); // modified
    bytes.push(0); // is_dir
    fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
    fs::write(&path, &bytes).expect("write cache");

    let error = Index::load(&path).expect_err("dangling directory index must be rejected");
    assert!(matches!(error, Error::CacheCorrupt(_)), "got: {error}");
}

#[test]
fn index_round_trips_through_serde_directly() {
    let dir = TempDir::new("cache-serde");
    let index = sample_index(dir.path());
    let text = serde_json::to_string(&index).expect("serialize");
    let loaded: Index = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(loaded.entries, index.entries);
    assert_eq!(loaded.roots, index.roots);
    assert_eq!(loaded.built_at, index.built_at);
    assert_eq!(loaded.search("hyjy", None, 10).expect("search").1, 1);
}

#[test]
fn cache_loads_a_document_written_by_the_go_implementation() {
    let dir = TempDir::new("cache-go");
    let path = cache_path(&dir);
    fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
    // Go marshals time.Time as RFC 3339 with a local offset and nanoseconds.
    let document = format!(
        r#"{{"version":1,"roots":["{root}"],"builtAt":"2023-11-15T06:13:20.123456789+08:00",
"entries":[{{"path":"{root}/会议纪要.docx","name":"会议纪要.docx","size":7,"modified":200,"isDir":false}}]}}"#,
        root = json_escape(&dir.path().to_string_lossy())
    );
    fs::write(&path, document).expect("write cache");

    let loaded = Index::load(&path).expect("load");
    assert_eq!(loaded.built_at, 1_700_000_000);
    assert_eq!(loaded.entries.len(), 1);
    assert_eq!(loaded.search("hyjy", None, 10).expect("search").1, 1);
}

#[test]
fn cache_accepts_a_numeric_timestamp_and_unknown_fields() {
    let dir = TempDir::new("cache-numeric");
    let path = cache_path(&dir);
    fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
    let document = format!(
        r#"{{"version":1,"roots":["{root}"],"builtAt":42,"future":"ignored",
"entries":[{{"path":"{root}/a.txt","name":"a.txt","size":1,"modified":0,"isDir":false,"extra":true}}]}}"#,
        root = json_escape(&dir.path().to_string_lossy())
    );
    fs::write(&path, document).expect("write cache");
    let loaded = Index::load(&path).expect("load");
    assert_eq!(loaded.built_at, 42);
    assert_eq!(loaded.entries.len(), 1);
}

#[test]
fn cache_entries_default_missing_optional_fields() {
    let dir = TempDir::new("cache-defaults");
    let path = cache_path(&dir);
    fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
    let document = format!(
        r#"{{"version":1,"entries":[{{"path":"{root}/a.txt","name":"a.txt"}}]}}"#,
        root = json_escape(&dir.path().to_string_lossy())
    );
    fs::write(&path, document).expect("write cache");
    let loaded = Index::load(&path).expect("load");
    assert_eq!(loaded.entries[0].size, 0);
    assert_eq!(loaded.entries[0].modified, 0);
    assert!(!loaded.entries[0].is_dir);
}

#[test]
fn cache_atomic_replacement_leaves_no_temporary_files() {
    let dir = TempDir::new("cache-atomic");
    let path = cache_path(&dir);
    let old = Index {
        entries: vec![Entry::new(
            dir.join("old.txt"),
            "old.txt".to_string(),
            3,
            0,
            false,
        )],
        roots: vec![dir.path().to_path_buf()],
        built_at: 1,
    };
    let new = Index {
        entries: vec![Entry::new(
            dir.join("new.txt"),
            "new.txt".to_string(),
            5,
            0,
            false,
        )],
        roots: vec![dir.path().to_path_buf()],
        built_at: 2,
    };
    old.save(&path).expect("save old");
    new.save(&path).expect("save new");

    let loaded = Index::load(&path).expect("load");
    assert_eq!(loaded.entries, new.entries);
    let count = fs::read_dir(path.parent().expect("parent"))
        .expect("read dir")
        .count();
    assert_eq!(count, 1, "replacement left temporary files behind");
}

#[test]
fn cache_cancelled_save_preserves_the_previous_index() {
    let dir = TempDir::new("cache-cancel");
    let path = cache_path(&dir);
    let old = Index {
        entries: vec![Entry::new(
            dir.join("old.txt"),
            "old.txt".to_string(),
            3,
            0,
            false,
        )],
        roots: vec![dir.path().to_path_buf()],
        built_at: 1,
    };
    old.save(&path).expect("save old");
    let before = fs::read(&path).expect("read old cache");

    let cancel = AtomicBool::new(true);
    let replacement = sample_index(dir.path());
    let error = replacement
        .save_cancellable(&cancel, &path)
        .expect_err("cancelled save must fail");
    assert!(matches!(error, Error::Cancelled), "got: {error}");

    let after = fs::read(&path).expect("read old cache");
    assert_eq!(before, after, "cancelled save changed the previous cache");
    assert_eq!(Index::load(&path).expect("load").entries, old.entries);
    let count = fs::read_dir(path.parent().expect("parent"))
        .expect("read dir")
        .count();
    assert_eq!(count, 1, "cancelled save left a temporary file behind");
}

#[cfg(unix)]
#[test]
fn cache_failed_save_preserves_the_previous_index() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("cache-readonly");
    let path = cache_path(&dir);
    let old = Index {
        entries: vec![Entry::new(
            dir.join("old.txt"),
            "old.txt".to_string(),
            3,
            0,
            false,
        )],
        roots: vec![dir.path().to_path_buf()],
        built_at: 1,
    };
    old.save(&path).expect("save old");
    let before = fs::read(&path).expect("read old cache");

    let cache_dir = path.parent().expect("parent").to_path_buf();
    fs::set_permissions(&cache_dir, fs::Permissions::from_mode(0o500)).expect("chmod");
    if fs::write(cache_dir.join("probe"), b"x").is_ok() {
        // Running as root: the directory is not actually unwritable.
        fs::set_permissions(&cache_dir, fs::Permissions::from_mode(0o700)).expect("chmod");
        let _ = fs::remove_file(cache_dir.join("probe"));
        return;
    }
    let result = sample_index(dir.path()).save(&path);
    fs::set_permissions(&cache_dir, fs::Permissions::from_mode(0o700)).expect("chmod");

    assert!(result.is_err(), "save succeeded in a read-only directory");
    assert_eq!(fs::read(&path).expect("read cache"), before);
    assert_eq!(Index::load(&path).expect("load").entries, old.entries);
}

#[test]
fn cache_rejects_corruption_and_incompatible_versions() {
    let dir = TempDir::new("cache-corrupt");
    let valid_path = cache_path(&dir);
    sample_index(dir.path()).save(&valid_path).expect("save");
    let valid = fs::read(&valid_path).expect("read valid cache");

    let root = json_escape(&dir.path().to_string_lossy()).to_string();
    let too_many_roots = format!(
        "[{}]",
        (0..65)
            .map(|_| format!("\"{root}\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("garbage", b"not a cache".to_vec()),
        ("truncated", valid[..valid.len() / 2].to_vec()),
        ("future-version", br#"{"version":2,"entries":[]}"#.to_vec()),
        ("missing-version", br#"{"entries":[]}"#.to_vec()),
        (
            "trailing-json",
            [valid.clone(), br#"{"extra":true}"#.to_vec()].concat(),
        ),
        (
            "too-many-roots",
            format!(r#"{{"version":1,"roots":{too_many_roots},"entries":[]}}"#).into_bytes(),
        ),
        (
            "missing-path",
            br#"{"version":1,"entries":[{"name":"a.txt"}]}"#.to_vec(),
        ),
        (
            "empty-path",
            br#"{"version":1,"entries":[{"path":"","name":"a.txt"}]}"#.to_vec(),
        ),
        (
            "missing-name",
            format!(r#"{{"version":1,"entries":[{{"path":"{root}/a.txt"}}]}}"#).into_bytes(),
        ),
        (
            "negative-size",
            format!(
                r#"{{"version":1,"entries":[{{"path":"{root}/a.txt","name":"a.txt","size":-1}}]}}"#
            )
            .into_bytes(),
        ),
        (
            "nul-path",
            format!(r#"{{"version":1,"entries":[{{"path":"{root}/a\u0000bad","name":"a.txt"}}]}}"#)
                .into_bytes(),
        ),
        (
            // The stored name is derived from the path, so the NUL has to be in
            // the path to be rejected.
            "nul-name",
            format!(r#"{{"version":1,"entries":[{{"path":"{root}/bad\u0000name","name":"bad\u0000name"}}]}}"#)
                .into_bytes(),
        ),
        (
            "entries-not-an-array",
            br#"{"version":1,"entries":{}}"#.to_vec(),
        ),
        (
            "roots-not-an-array",
            br#"{"version":1,"roots":{},"entries":[]}"#.to_vec(),
        ),
        (
            "duplicate-version",
            br#"{"version":1,"version":1,"entries":[]}"#.to_vec(),
        ),
        (
            "bad-timestamp",
            br#"{"version":1,"builtAt":"yesterday","entries":[]}"#.to_vec(),
        ),
        (
            "entry-too-long-name",
            format!(
                r#"{{"version":1,"entries":[{{"path":"{root}/a.txt","name":"{}"}}]}}"#,
                "n".repeat(1025)
            )
            .into_bytes(),
        ),
    ];

    for (label, data) in cases {
        let path = dir.join(&format!("{label}.json"));
        fs::write(&path, &data).expect("write corrupt cache");
        let loaded = Index::load(&path);
        assert!(loaded.is_err(), "load accepted corrupted cache: {label}");
    }

    assert!(Index::load(&dir.join("does-not-exist.json")).is_err());
    assert!(
        Index::load(dir.path()).is_err(),
        "a directory is not a cache"
    );
}

#[test]
fn cache_rejects_an_oversized_file_before_decoding() {
    let dir = TempDir::new("cache-oversized");
    let path = dir.join("oversized.json");
    let file = fs::File::create(&path).expect("create");
    if file.set_len(MAX_CACHE_BYTES + 1).is_err() {
        return;
    }
    drop(file);
    let error = Index::load(&path).expect_err("oversized cache");
    assert!(matches!(error, Error::CacheTooLarge), "got: {error}");
}

#[cfg(unix)]
#[test]
fn cache_refuses_a_symlinked_file() {
    let dir = TempDir::new("cache-symlink");
    let real = dir.join("real.json");
    sample_index(dir.path()).save(&real).expect("save");
    let link = dir.join("link.json");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let error = Index::load(&link).expect_err("symlinked cache");
    assert!(matches!(error, Error::CacheUnsupported(_)), "got: {error}");
}

#[cfg(unix)]
#[test]
fn cache_file_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("cache-perms");
    let path = cache_path(&dir);
    sample_index(dir.path()).save(&path).expect("save");
    let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "cache file mode was {mode:o}");
}

#[test]
fn cache_save_validates_entries_and_roots() {
    let dir = TempDir::new("cache-invalid");
    let path = cache_path(&dir);

    let empty_path = Index {
        entries: vec![Entry::new(PathBuf::new(), "a.txt".to_string(), 1, 0, false)],
        roots: Vec::new(),
        built_at: 0,
    };
    assert!(empty_path.save(&path).is_err());

    // The root directory has no file name, which is what an empty name means.
    let empty_name = Index {
        entries: vec![Entry::new(PathBuf::from("/"), String::new(), 1, 0, false)],
        roots: Vec::new(),
        built_at: 0,
    };
    assert!(empty_name.save(&path).is_err());

    let long_name = Index {
        entries: vec![Entry::new(
            dir.join(&"n".repeat(1025)),
            String::new(),
            1,
            0,
            false,
        )],
        roots: Vec::new(),
        built_at: 0,
    };
    assert!(long_name.save(&path).is_err());

    let too_many_roots = Index {
        entries: Vec::new(),
        roots: (0..65).map(|_| dir.path().to_path_buf()).collect(),
        built_at: 0,
    };
    assert!(too_many_roots.save(&path).is_err());

    let empty_root = Index {
        entries: Vec::new(),
        roots: vec![PathBuf::new()],
        built_at: 0,
    };
    assert!(empty_root.save(&path).is_err());

    // A valid index with no entries still round-trips.
    let empty = Index {
        entries: Vec::new(),
        roots: vec![dir.path().to_path_buf()],
        built_at: 5,
    };
    empty.save(&path).expect("save empty");
    let loaded = Index::load(&path).expect("load empty");
    assert!(loaded.entries.is_empty());
    assert_eq!(loaded.roots, empty.roots);
    assert_eq!(loaded.built_at, 5);
    assert_eq!(loaded.search("anything", None, 10).expect("search").1, 0);
}

#[test]
fn cache_survives_a_large_entry_set() {
    let dir = TempDir::new("cache-large");
    let path = cache_path(&dir);
    let entries: Vec<Entry> = (0..20_000)
        .map(|index| {
            let name = format!("file-{index:05}.txt");
            Entry::new(dir.join(&name), name, index as u64, 0, false)
        })
        .collect();
    let index = Index {
        entries,
        roots: vec![dir.path().to_path_buf()],
        built_at: 7,
    };
    index.save(&path).expect("save");
    let loaded = Index::load(&path).expect("load");
    assert_eq!(loaded.entries.len(), 20_000);
    let (results, total) = loaded.search("file-00", None, 100).expect("search");
    assert_eq!(total, 1_000);
    assert_eq!(results.len(), 100);
    assert_eq!(
        sorted_expected(&["file-00000.txt"]),
        common::sorted_names(&results[..1])
    );
}
