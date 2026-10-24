//! Query grammar, filtering, ranking and limit behaviour.

mod common;

use common::{filtered_names, names, search_fixture, sorted_expected, sorted_names, TempDir};
use oilfind_core::{Entry, Index};
use std::path::Path;

#[test]
fn search_text_and_boolean_operators() {
    let (_dir, index) = search_fixture();
    let cases: &[(&str, &[&str])] = &[
        (
            "rEpOrT",
            &[
                "Report Final.PDF",
                "report draft.docx",
                "report notes.txt",
                "reports",
                "report old.pdf",
            ],
        ),
        ("会议", &["会议纪要.docx"]),
        ("huiyijiyao", &["会议纪要.docx"]),
        ("hyjy", &["会议纪要.docx"]),
        ("HUIYI", &["会议纪要.docx"]),
        ("report final", &["Report Final.PDF"]),
        ("report photo", &[]),
        ("会议 | photo", &["会议纪要.docx", "photo.jpg"]),
        ("report final | photo", &["Report Final.PDF", "photo.jpg"]),
        (
            "report !draft",
            &[
                "Report Final.PDF",
                "report notes.txt",
                "reports",
                "report old.pdf",
            ],
        ),
        (
            "report !archive",
            &[
                "Report Final.PDF",
                "report draft.docx",
                "report notes.txt",
                "reports",
            ],
        ),
        (
            "report !archive | photo",
            &[
                "Report Final.PDF",
                "report draft.docx",
                "report notes.txt",
                "reports",
                "photo.jpg",
            ],
        ),
        ("path:archive", &["report old.pdf"]),
        ("unfindable-needle", &[]),
    ];
    for (query, expected) in cases {
        assert_eq!(
            names(&index, query),
            sorted_expected(expected),
            "query {query:?}"
        );
    }
}

#[test]
fn search_filters() {
    let (_dir, index) = search_fixture();
    let today_query = format!("dm:{}", common::today_local());
    let cases: Vec<(String, Vec<&str>)> = vec![
        (
            "ext:PDF".to_string(),
            vec!["Report Final.PDF", "预算报告.pdf", "report old.pdf"],
        ),
        (
            "ext:pdf;docx".to_string(),
            vec![
                "Report Final.PDF",
                "report draft.docx",
                "会议纪要.docx",
                "预算报告.pdf",
                "report old.pdf",
            ],
        ),
        ("kind:image".to_string(), vec!["IMG_001.PNG", "photo.jpg"]),
        ("kind:folder".to_string(), vec!["reports"]),
        ("kind:code".to_string(), vec!["main.go"]),
        (
            "file:report".to_string(),
            vec![
                "Report Final.PDF",
                "report draft.docx",
                "report notes.txt",
                "report old.pdf",
            ],
        ),
        ("folder:report".to_string(), vec!["reports"]),
        (
            "size:>10mb".to_string(),
            vec!["Report Final.PDF", "report old.pdf"],
        ),
        (
            "size:1mb..5mb".to_string(),
            vec![
                "report draft.docx",
                "IMG_001.PNG",
                "会议纪要.docx",
                "预算报告.pdf",
            ],
        ),
        (
            "dm:today".to_string(),
            vec![
                "Report Final.PDF",
                "IMG_001.PNG",
                "会议纪要.docx",
                "reports",
            ],
        ),
        (
            "dm:week".to_string(),
            vec![
                "Report Final.PDF",
                "IMG_001.PNG",
                "会议纪要.docx",
                "reports",
            ],
        ),
        (
            today_query,
            vec![
                "Report Final.PDF",
                "IMG_001.PNG",
                "会议纪要.docx",
                "reports",
            ],
        ),
        ("*.png".to_string(), vec!["IMG_001.PNG"]),
        ("regex:^IMG_".to_string(), vec!["IMG_001.PNG"]),
        (
            "report ext:pdf size:>10mb !archive".to_string(),
            vec!["Report Final.PDF"],
        ),
        (
            "size:>=3mb".to_string(),
            vec![
                "Report Final.PDF",
                "report draft.docx",
                "预算报告.pdf",
                "report old.pdf",
            ],
        ),
        (
            "size:<1mb".to_string(),
            vec!["report notes.txt", "photo.jpg", "main.go"],
        ),
        ("size:2048".to_string(), vec!["main.go"]),
        ("size:2kb".to_string(), vec!["main.go"]),
    ];
    for (query, expected) in cases {
        assert_eq!(
            names(&index, &query),
            sorted_expected(&expected),
            "query {query:?}"
        );
    }
}

#[test]
fn document_kind_covers_office_formats() {
    let dir = TempDir::new("kinds");
    let root = dir.path().to_path_buf();
    let extensions = [
        "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "rtf", "csv", "odt",
        "ods", "odp", "wps", "et", "dps", "ofd",
    ];
    let mut entries: Vec<Entry> = extensions
        .iter()
        .map(|ext| {
            let name = format!("a.{ext}");
            Entry::new(root.join(&name), name, 1, 0, false)
        })
        .collect();
    entries.push(Entry::new(
        root.join("c.png"),
        "c.png".to_string(),
        1,
        0,
        false,
    ));
    entries.push(Entry::new(
        root.join("d.go"),
        "d.go".to_string(),
        1,
        0,
        false,
    ));
    entries.push(Entry::new(
        root.join("folder"),
        "folder".to_string(),
        0,
        0,
        true,
    ));
    let index = Index {
        entries,
        roots: vec![root],
        built_at: 0,
    };

    let found = names(&index, "kind:doc");
    assert_eq!(found.len(), extensions.len());
    assert!(!found.contains(&"c.png".to_string()));
    assert!(!found.contains(&"d.go".to_string()));
    assert!(!found.contains(&"folder".to_string()));
}

#[test]
fn invalid_syntax_is_rejected() {
    let (_dir, index) = search_fixture();
    let queries = [
        "regex:[",
        "regex:(",
        "size:>not-a-size",
        "size:5mb..1mb",
        "dm:not-a-date",
        "dm:2024-02-30",
        "report |",
        "| report",
        "\"\"",
        "!",
        "kind:nope",
        "ext:",
        "path:",
        "regex:",
    ];
    for query in queries {
        assert!(
            index.search(query, None, 20).is_err(),
            "query {query:?} should be rejected"
        );
    }
    assert!(
        index.search("", None, 20).is_ok(),
        "empty query matches all"
    );
}

#[test]
fn glob_treats_bracket_as_a_literal() {
    let dir = TempDir::new("bracket");
    let root = dir.path().to_path_buf();
    let index = Index {
        entries: vec![
            Entry::new(root.join("[a].txt"), "[a].txt".to_string(), 1, 0, false),
            Entry::new(root.join("a.txt"), "a.txt".to_string(), 1, 0, false),
        ],
        roots: vec![root],
        built_at: 0,
    };
    assert_eq!(names(&index, "[a].txt"), vec!["[a].txt".to_string()]);
    assert_eq!(names(&index, "[a].*"), vec!["[a].txt".to_string()]);
}

#[test]
fn quoted_phrases_and_or_groups() {
    let dir = TempDir::new("quotes");
    let root = dir.path().to_path_buf();
    let index = Index {
        entries: vec![
            Entry::new(
                root.join("annual report.txt"),
                "annual report.txt".to_string(),
                1,
                0,
                false,
            ),
            Entry::new(
                root.join("annual.txt"),
                "annual.txt".to_string(),
                1,
                0,
                false,
            ),
            Entry::new(
                root.join("report.txt"),
                "report.txt".to_string(),
                1,
                0,
                false,
            ),
        ],
        roots: vec![root],
        built_at: 0,
    };
    assert_eq!(
        names(&index, "\"annual report\""),
        vec!["annual report.txt".to_string()]
    );
    assert_eq!(
        names(&index, "annual | report"),
        vec![
            "annual report.txt".to_string(),
            "annual.txt".to_string(),
            "report.txt".to_string()
        ]
    );
}

#[test]
fn ranking_prefers_exact_then_prefix_then_substring() {
    let dir = TempDir::new("ranking");
    let root = dir.path().to_path_buf();
    let entries = vec![
        Entry::new(root.join("z/xreport"), "xreport".to_string(), 1, 0, false),
        Entry::new(root.join("b/report"), "report".to_string(), 1, 0, false),
        Entry::new(
            root.join("z/report-extra"),
            "report-extra".to_string(),
            1,
            0,
            false,
        ),
        Entry::new(root.join("a/report"), "report".to_string(), 1, 0, false),
    ];
    let expected = vec![
        entries[3].path(),
        entries[1].path(),
        entries[2].path(),
        entries[0].path(),
    ];
    let index = Index {
        entries,
        roots: vec![root],
        built_at: 0,
    };
    for _ in 0..3 {
        let (results, total) = index.search("report", None, 100).expect("search");
        assert_eq!(total, expected.len());
        let paths: Vec<_> = results.iter().map(|entry| entry.path()).collect();
        assert_eq!(paths, expected);
    }
}

#[test]
fn ranking_prefers_name_over_extension_and_pinyin() {
    let dir = TempDir::new("ranking2");
    let root = dir.path().to_path_buf();
    let index = Index {
        entries: vec![
            Entry::new(
                root.join("notes/报告.txt"),
                "报告.txt".to_string(),
                1,
                0,
                false,
            ),
            Entry::new(root.join("报告.txt"), "报告.txt".to_string(), 1, 0, false),
            Entry::new(
                root.join("a/其他报告.txt"),
                "其他报告.txt".to_string(),
                1,
                0,
                false,
            ),
        ],
        roots: vec![root],
        built_at: 0,
    };
    // Both "报告.txt" entries score 90 (name without extension), the substring
    // hit scores 30, so the extension-exact names always come first.
    let (results, total) = index.search("报告", None, 100).expect("search");
    assert_eq!(total, 3);
    assert_eq!(results[0].name(), "报告.txt");
    assert_eq!(results[1].name(), "报告.txt");
    assert_eq!(results[2].name(), "其他报告.txt");
    // Pinyin initials and full pinyin both reach the same entries.
    assert_eq!(names(&index, "bg").len(), 3);
    assert_eq!(names(&index, "qitabaogao"), vec!["其他报告.txt"]);
}

#[test]
fn limit_caps_results_but_not_the_total() {
    let dir = TempDir::new("limit");
    let root = dir.path().to_path_buf();
    const COUNT: usize = 10_000;
    let entries: Vec<Entry> = (0..COUNT)
        .map(|index| {
            let name = format!("record-{:05}.txt", COUNT - index - 1);
            Entry::new(root.join(&name), name, index as u64, 0, false)
        })
        .collect();
    let index = Index {
        entries,
        roots: vec![root],
        built_at: 0,
    };
    for limit in [1usize, 7, 100, COUNT + 1] {
        let (results, total) = index.search("record", None, limit).expect("search");
        let expected = limit.min(COUNT);
        assert_eq!(total, COUNT, "limit {limit}");
        assert_eq!(results.len(), expected, "limit {limit}");
        for (position, entry) in results.iter().enumerate() {
            assert_eq!(entry.name(), format!("record-{position:05}.txt"));
        }
    }
    // The default and the ceiling both keep the total intact.
    let (results, total) = index.search("record", None, 0).expect("search");
    assert_eq!(total, COUNT);
    assert_eq!(results.len(), 2000);
    let (results, _) = index.search("record", None, usize::MAX).expect("search");
    assert_eq!(results.len(), 10_000);
}

#[test]
fn category_filter_cannot_be_bypassed_by_or() {
    let (_dir, index) = search_fixture();
    let cases: &[(&str, &str, &[&str])] = &[
        ("report | photo", "image", &["photo.jpg"]),
        (
            "report | photo",
            "doc",
            &[
                "Report Final.PDF",
                "report draft.docx",
                "report notes.txt",
                "report old.pdf",
            ],
        ),
        ("report | photo", "folder", &["reports"]),
        ("report | main", "code", &["main.go"]),
        (
            "report | photo",
            "file",
            &[
                "Report Final.PDF",
                "report draft.docx",
                "report notes.txt",
                "report old.pdf",
                "photo.jpg",
            ],
        ),
        ("kind:folder | kind:image", "doc", &[]),
        (
            "kind:folder | kind:image",
            "image",
            &["IMG_001.PNG", "photo.jpg"],
        ),
        ("!photo | report", "image", &["IMG_001.PNG"]),
        ("", "image", &["IMG_001.PNG", "photo.jpg"]),
    ];
    for (query, kind, expected) in cases {
        assert_eq!(
            filtered_names(&index, query, kind),
            sorted_expected(expected),
            "query {query:?} kind {kind:?}"
        );
    }
}

#[test]
fn category_limit_and_validation() {
    let (_dir, index) = search_fixture();
    let (results, total) = index
        .search("report | photo | IMG_", Some("image"), 1)
        .expect("search");
    assert_eq!(total, 2);
    assert_eq!(results.len(), 1);
    assert!(!results[0].is_dir);
    assert!(matches!(results[0].name(), "photo.jpg" | "IMG_001.PNG"));

    let (plain, plain_total) = index.search("report | photo", None, 100).expect("search");
    let (filtered, filtered_total) = index
        .search("report | photo", Some(""), 100)
        .expect("search");
    assert_eq!(filtered_total, plain_total);
    assert_eq!(
        sorted_names(&filtered),
        sorted_names(&plain),
        "an empty category behaves like no category"
    );

    assert!(index
        .search("report", Some("unknown-category"), 100)
        .is_err());
    assert!(index.search("regex:[", Some("image"), 100).is_err());
}

#[test]
fn search_never_reads_file_contents() {
    let dir = TempDir::new("metadata-only");
    let root = dir.path().to_path_buf();
    let path = root.join("not-created.txt");
    let index = Index {
        entries: vec![Entry::new(path, "not-created.txt".to_string(), 0, 0, false)],
        roots: vec![root],
        built_at: 0,
    };
    assert_eq!(names(&index, "not-created"), vec!["not-created.txt"]);
}

#[test]
fn oversized_queries_and_too_many_terms_are_rejected() {
    let (_dir, index) = search_fixture();
    let long = "a".repeat(4097);
    assert!(index.search(&long, None, 10).is_err());
    let wide = (0..129)
        .map(|index| format!("t{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(index.search(&wide, None, 10).is_err());
    let boundary = (0..128)
        .map(|index| format!("t{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(index.search(&boundary, None, 10).is_ok());
}

#[test]
fn path_filters_accept_backslashes() {
    let dir = TempDir::new("path-filter");
    let root = dir.path().to_path_buf();
    let index = Index {
        entries: vec![Entry::new(
            root.join("a/b/c.txt"),
            "c.txt".to_string(),
            1,
            0,
            false,
        )],
        roots: vec![root],
        built_at: 0,
    };
    assert_eq!(names(&index, "path:a/b"), vec!["c.txt"]);
    assert_eq!(names(&index, r"path:a\b"), vec!["c.txt"]);
}

#[test]
fn entries_from_serde_arrive_prepared() {
    // Deserializing rebuilds the derived fields immediately, so an entry that
    // did not come from `Index::load` still searches.
    let entry: Entry = serde_json::from_str(
        r#"{"path":"/x/会议纪要.docx","name":"会议纪要.docx","size":1,"modified":0,"isDir":false}"#,
    )
    .expect("deserialize entry");
    assert_eq!(entry.name(), "会议纪要.docx");
    assert_eq!(entry.lower_name(), "会议纪要.docx");
    assert_eq!(entry.full_pinyin(), "huiyijiyao.docx");
    let index = Index {
        entries: vec![entry],
        roots: vec![Path::new("/x").to_path_buf()],
        built_at: 0,
    };
    assert_eq!(names(&index, "hyjy"), vec!["会议纪要.docx"]);
}
