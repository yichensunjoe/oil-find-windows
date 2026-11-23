use std::fs;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use crate::content::ContentIndex;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let base = crate::platform::resolve(&std::env::temp_dir()).unwrap();
        // A volume-root /tmp is intentionally excluded by the scanner. Use
        // the other system temporary directory on Linux without changing policy.
        #[cfg(target_os = "linux")]
        let base = if oilfind_core::validate_scan_roots(std::slice::from_ref(&base)).is_ok() {
            base
        } else {
            crate::platform::resolve(std::path::Path::new("/var/tmp")).unwrap()
        };
        loop {
            let path = base.join(format!(
                "oilfind-content-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => panic!("cannot create fixture: {err}"),
            }
        }
    }

    fn write(&self, name: &str, data: impl AsRef<[u8]>) {
        fs::write(self.0.join(name), data).unwrap();
    }

    fn scan(&self) -> ContentIndex {
        self.refresh(None)
    }

    fn refresh(&self, previous: Option<&ContentIndex>) -> ContentIndex {
        self.try_refresh(previous).unwrap()
    }

    fn try_refresh(&self, previous: Option<&ContentIndex>) -> Result<ContentIndex, String> {
        let roots = std::slice::from_ref(&self.0);
        #[cfg(windows)]
        let volumes = crate::platform::volume_ids(roots).unwrap();
        #[cfg(not(windows))]
        let volumes = Default::default();
        ContentIndex::refresh(&AtomicBool::new(false), roots, &volumes, previous)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn exact_phrase_returns_each_file_once_and_selects_the_following_hit() {
    let fixture = Fixture::new();
    for i in 0..10 {
        fixture.write(
            &format!("资料{i}.txt"),
            "题目\r\n🌊 海水淡化是新增水源。\r\n备选海水淡化工艺。",
        );
    }
    fixture.write("海水淡化.md", "海水，淡化（分开）。");
    let index = fixture.scan();
    let (hits, total) = index.search("海水淡化", 2000, |_, _| std::cmp::Ordering::Equal);
    assert_eq!(total, 10);
    assert_eq!(hits[0].count, 2);
    assert_eq!(hits[0].location, 2);
    assert!(hits[0].snippet.contains("【海水淡化】"));

    let doc = index.document(&hits[0]);
    let first = doc.preview("海水淡化", 0).unwrap();
    assert_eq!(first.location, 2);
    assert_eq!(first.selection, 7..11);
    let next = doc.preview("海水淡化", 1).unwrap();
    assert_eq!(next.location, 3);
    let units: Vec<u16> = next.text.encode_utf16().collect();
    assert_eq!(
        String::from_utf16(&units[next.selection]).unwrap(),
        "海水淡化"
    );
    assert!(doc.preview("海水淡化", 2).is_none());
}

#[test]
fn word_phrase_spanning_formatting_runs_points_to_its_paragraph() {
    let fixture = Fixture::new();
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "word/document.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:body><w:p><w:r><w:t>标题</w:t></w:r></w:p>
        <w:p><w:r><w:t>海水</w:t></w:r><w:r><w:t>淡化 &amp; 回用</w:t></w:r></w:p>
        </w:body></w:document>"#
            .as_bytes(),
    )
    .unwrap();
    fixture.write("报告.docx", zip.finish().unwrap().into_inner());
    let index = fixture.scan();
    let (hits, total) = index.search("海水淡化", 10, |_, _| std::cmp::Ordering::Equal);
    assert_eq!(total, 1);
    assert_eq!(hits[0].count, 1);
    assert_eq!(hits[0].location, 2);
    let preview = index.document(&hits[0]).preview("海水淡化", 0).unwrap();
    assert!(preview.text.contains("标题"));
    assert!(preview.text.contains("海水淡化 & 回用"));
}

#[test]
fn missing_root_is_an_error_and_cancelled_scan_returns_no_snapshot() {
    let fixture = Fixture::new();
    let absent = fixture.0.join("absent");
    assert!(ContentIndex::refresh(
        &AtomicBool::new(false),
        &[absent],
        &Default::default(),
        None
    )
    .is_err());
    assert!(ContentIndex::refresh(
        &AtomicBool::new(true),
        std::slice::from_ref(&fixture.0),
        &Default::default(),
        None
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn only_explicit_scope_is_read_and_links_are_ignored() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let leaked = "其他位置的正文";
    outside.write("外部.txt", leaked);
    fixture.write("已选.txt", "明确选中的正文");
    std::os::unix::fs::symlink(outside.0.join("外部.txt"), fixture.0.join("链接.txt")).unwrap();
    std::os::unix::fs::symlink(&outside.0, fixture.0.join("目录链接")).unwrap();
    let index = fixture.scan();
    let (_, total) = index.search(leaked, 10, |_, _| std::cmp::Ordering::Equal);
    assert_eq!(total, 0);
    assert_eq!(index.len(), 1);
}

#[cfg(unix)]
#[test]
fn moving_an_indexed_folder_and_installing_a_link_drops_its_cached_bodies() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let folder = fixture.0.join("reports");
    let destination = outside.0.join("reports");
    fs::create_dir(&folder).unwrap();
    fixture.write("reports/local.txt", "已索引的正文");
    let previous = fixture.scan();
    fs::rename(&folder, &destination).unwrap();
    std::os::unix::fs::symlink(&destination, &folder).unwrap();

    let refreshed = fixture.refresh(Some(&previous));
    assert_eq!(refreshed.len(), 0);
    assert_eq!(refreshed.report().unwrap().reused, 0);
}

#[cfg(unix)]
#[test]
fn unreadable_indexed_subdirectory_does_not_drop_previous_results() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let folder = fixture.0.join("reports");
    fs::create_dir(&folder).unwrap();
    fixture.write("reports/local.txt", "已索引的正文");
    let previous = fixture.scan();
    let permissions = fs::metadata(&folder).unwrap().permissions();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o111)).unwrap();
    let unreadable = fs::read_dir(&folder).is_err();
    let refreshed = fixture.try_refresh(Some(&previous));
    fs::set_permissions(&folder, permissions).unwrap();

    if unreadable {
        assert!(refreshed.is_err());
        assert_eq!(previous.len(), 1);
    }
}

#[test]
fn cached_body_requires_the_same_approved_scope() {
    let folder = Fixture::new();
    let unrelated = Fixture::new();
    folder.write("内部.txt", "海水淡化");
    let data = serde_json::to_vec(&folder.scan()).unwrap();
    folder.write("content-cache.json", data);
    let stored =
        crate::content::read_local_file(&folder.0.join("content-cache.json"), 1024 * 1024).unwrap();
    let restored = ContentIndex::from_cache(&stored, std::slice::from_ref(&folder.0)).unwrap();
    assert_eq!(restored.len(), 1);
    for roots in [&[][..], std::slice::from_ref(&unrelated.0)] {
        assert!(ContentIndex::from_cache(&stored, roots).is_err());
    }
}

#[test]
fn changing_roots_does_not_reuse_even_a_still_in_scope_file() {
    let fixture = Fixture::new();
    let child = fixture.0.join("scope");
    fs::create_dir(&child).unwrap();
    fixture.write("scope/present.txt", "正文");
    let previous = fixture.scan();
    let roots = &[child];
    #[cfg(windows)]
    let volumes = crate::platform::volume_ids(roots).unwrap();
    #[cfg(not(windows))]
    let volumes = Default::default();
    let index =
        ContentIndex::refresh(&AtomicBool::new(false), roots, &volumes, Some(&previous)).unwrap();
    let report = index.report().unwrap();
    assert_eq!((report.reused, report.extracted), (0, 1));
}

#[test]
fn refresh_reuses_unchanged_text_from_disk_and_updates_the_snapshot() {
    let fixture = Fixture::new();
    fixture.write("a.txt", "索引·不变");
    fixture.write("b.txt", "索引·旧版");
    fixture.write("c.txt", "索引·删除");
    let bytes = serde_json::to_vec(&fixture.scan()).unwrap();
    let previous = ContentIndex::from_cache(&bytes, std::slice::from_ref(&fixture.0)).unwrap();
    fixture.write("b.txt", "索引·修改后");
    fs::remove_file(fixture.0.join("c.txt")).unwrap();
    fixture.write("d.txt", "索引·新增");

    let refreshed = fixture.refresh(Some(&previous));
    let report = refreshed.report().unwrap();
    assert_eq!((report.reused, report.extracted), (1, 2));
    assert_eq!(refreshed.skipped, 0);
    let (hits, count) = refreshed.search("索引", 10, |_, _| std::cmp::Ordering::Equal);
    assert_eq!(count, 3);
    let names: Vec<_> = hits
        .iter()
        .map(|hit| refreshed.document(hit).name())
        .collect();
    assert_eq!(names, ["a.txt", "b.txt", "d.txt"]);
    assert_eq!(
        refreshed
            .search("旧版", 10, |_, _| std::cmp::Ordering::Equal)
            .1,
        0
    );
}

#[test]
fn same_length_and_modified_time_cannot_reuse_a_replacement_file() {
    let fixture = Fixture::new();
    fixture.write("document.txt", "before");
    let previous = fixture.scan();
    let path = fixture.0.join("document.txt");
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let replacement = fixture.0.join("replacement.tmp");
    fixture.write("replacement.tmp", "update");
    fs::OpenOptions::new()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    fs::remove_file(&path).unwrap();
    fs::rename(replacement, &path).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);

    let (hits, _) = previous.search("before", 10, |_, _| std::cmp::Ordering::Equal);
    assert!(!previous
        .document(&hits[0])
        .matches_file(&fs::File::open(&path).unwrap()));
    let refreshed = fixture.refresh(Some(&previous));
    let report = refreshed.report().unwrap();
    assert_eq!((report.reused, report.extracted), (0, 1));
    assert_eq!(
        refreshed
            .search("update", 10, |_, _| std::cmp::Ordering::Equal)
            .1,
        1
    );
}

#[test]
fn legacy_cache_without_identity_is_searchable_and_refreshes_once() {
    let fixture = Fixture::new();
    fixture.write("source.txt", "正文");
    let mut value = serde_json::to_value(fixture.scan()).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("report");
    object.remove("skipped_sources");
    object.get_mut("documents").unwrap()[0]["stamp"]
        .as_object_mut()
        .unwrap()
        .remove("identity");
    let bytes = serde_json::to_vec(&value).unwrap();
    let previous = ContentIndex::from_cache(&bytes, std::slice::from_ref(&fixture.0)).unwrap();
    assert!(previous.report().is_none());
    assert_eq!(
        previous
            .search("正文", 10, |_, _| std::cmp::Ordering::Equal)
            .1,
        1
    );
    let first = fixture.refresh(Some(&previous));
    assert_eq!(first.report().unwrap().extracted, 1);
    assert_eq!(fixture.refresh(Some(&first)).report().unwrap().reused, 1);
}

#[test]
fn refresh_distinguishes_image_only_and_empty_text_from_limits_and_invalid_documents() {
    let fixture = Fixture::new();
    fixture.write("empty.txt", " \r\n\t");
    fixture.write("photo.pdf", image_pdf());
    fixture.write("invalid.pdf", "not a PDF");
    fixture.write("invalid.docx", "not a DOCX");
    fixture.write("over-text-limit.md", vec![b'a'; 2 * 1024 * 1024 + 1]);
    fs::File::create(fixture.0.join("over-file-limit.pdf"))
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    fixture.write("contents.txt", "正文");

    let index = fixture.scan();
    let report = index.report().unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index.skipped, 6);
    assert_eq!(report.no_text, 2);
    assert_eq!(report.extraction_failed, 2);
    assert_eq!(report.too_large, 2);
    assert_eq!(report.inaccessible, 0);
    assert_eq!(report.unavailable, 0);

    let bytes = serde_json::to_vec(&index).unwrap();
    let restored = ContentIndex::from_cache(&bytes, std::slice::from_ref(&fixture.0)).unwrap();
    assert_eq!(restored.report(), Some(report));
    let refreshed = fixture.refresh(Some(&restored)).report().unwrap();
    assert_eq!(refreshed.reused, 1);
    assert_eq!(refreshed.extracted, 0);
    assert_eq!(
        (
            refreshed.no_text,
            refreshed.extraction_failed,
            refreshed.too_large
        ),
        (2, 2, 2)
    );
}

#[test]
fn sorting_counts_all_hits_before_truncating() {
    let fixture = Fixture::new();
    fixture.write("a.txt", "海水淡化");
    fixture.write("b.txt", "海水淡化，海水淡化");
    let (hits, total) = fixture
        .scan()
        .search("海水淡化", 1, |a, b| b.count.cmp(&a.count));
    assert_eq!(total, 2);
    assert_eq!(hits[0].count, 2);
}

#[test]
fn edited_document_does_not_match_the_cached_snapshot() {
    let fixture = Fixture::new();
    fixture.write("实验.txt", "海水淡化");
    let index = fixture.scan();
    let (hits, _) = index.search("海水淡化", 10, |_, _| std::cmp::Ordering::Equal);
    let document = index.document(&hits[0]);
    assert!(document.matches_metadata(&fs::metadata(&document.path).unwrap()));
    fixture.write("实验.txt", "已重写");
    assert!(!document.matches_metadata(&fs::metadata(&document.path).unwrap()));
}

#[cfg(windows)]
#[test]
fn rebuilding_the_index_requires_the_expected_volume_guid() {
    let fixture = Fixture::new();
    fixture.write("实验.txt", "海水淡化");
    let roots = std::slice::from_ref(&fixture.0);
    let mut volumes = crate::platform::volume_ids(roots).unwrap();
    for volume in volumes.values_mut() {
        *volume = r"\\?\Volume{00000000-0000-0000-0000-000000000000}\".to_string();
    }
    let previous = fixture.scan();
    assert!(
        ContentIndex::refresh(&AtomicBool::new(false), roots, &volumes, Some(&previous)).is_err()
    );
    assert_eq!(previous.len(), 1);
}

#[cfg(windows)]
#[test]
fn refresh_cannot_publish_an_empty_snapshot_when_an_indexed_file_is_locked() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = Fixture::new();
    fixture.write("实验.txt", "海水淡化");
    let previous = fixture.scan();
    let roots = std::slice::from_ref(&fixture.0);
    let volumes = crate::platform::volume_ids(roots).unwrap();
    let path = fixture.0.join("实验.txt");
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let refresh = ContentIndex::refresh(&AtomicBool::new(false), roots, &volumes, Some(&previous));
    assert!(refresh.is_err());
    assert_eq!(
        previous
            .search("海水淡化", 10, |_, _| std::cmp::Ordering::Equal)
            .1,
        1
    );
}

#[cfg(windows)]
#[test]
fn a_new_locked_file_has_its_own_skip_reason() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = Fixture::new();
    fixture.write("locked.txt", "正文");
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(fixture.0.join("locked.txt"))
        .unwrap();
    let index = fixture.scan();
    assert_eq!(index.len(), 0);
    assert_eq!(index.skipped, 1);
    assert_eq!(index.report().unwrap().inaccessible, 1);
}

#[test]
fn windows_text_encodings_preserve_the_exact_characters() {
    let fixture = Fixture::new();
    fixture.write("ansi.txt", [0xba, 0xa3, 0xcb, 0xae, 0xb5, 0xad, 0xbb, 0xaf]);
    let utf16 = "海水淡化"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    fixture.write("unicode.txt", [vec![0xff, 0xfe], utf16].concat());
    let index = fixture.scan();
    assert_eq!(
        index
            .search("海水淡化", 10, |_, _| std::cmp::Ordering::Equal)
            .1,
        2
    );
}

// A PDF page with a Unicode CMap, matching how Chinese text is encoded by
// ordinary PDF producers. The second page contains the only occurrence.
#[test]
fn pdf_hit_uses_the_page_number_and_highlights_decoded_chinese_text() {
    let fixture = Fixture::new();
    fixture.write("设计.pdf", chinese_pdf());
    let index = fixture.scan();
    let (hits, total) = index.search("海水淡化", 10, |_, _| std::cmp::Ordering::Equal);
    assert_eq!(total, 1);
    assert_eq!(hits[0].count, 1);
    assert_eq!(hits[0].location, 2);
    let doc = index.document(&hits[0]);
    let (page, text, selection) = doc.pdf_match("海水淡化", 0).unwrap();
    assert_eq!(page, 2);
    assert_eq!(&text[selection], "海水淡化");
    assert!(doc.pdf_match("海水淡化", 1).is_none());
    let preview = doc.preview("海水淡化", 0).unwrap();
    assert_eq!(preview.location, 2);
    assert_eq!(
        doc.path.file_name().and_then(std::ffi::OsStr::to_str),
        Some("设计.pdf")
    );
    let units: Vec<u16> = preview.text.encode_utf16().collect();
    assert_eq!(
        String::from_utf16(&units[preview.selection]).unwrap(),
        "海水淡化"
    );
}

fn image_pdf() -> Vec<u8> {
    use lopdf::{dictionary, Document, Stream};
    let mut pdf = Document::with_version("1.5");
    let pages = pdf.new_object_id();
    let image = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
            "BitsPerComponent" => 8, "ColorSpace" => "DeviceGray",
        },
        vec![0],
    ));
    let contents = pdf.add_object(Stream::new(dictionary! {}, b"/Image Do".to_vec()));
    let page = pdf.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages, "Contents" => contents,
        "Resources" => dictionary! { "XObject" => dictionary! { "Image" => image } },
    });
    pdf.set_object(
        pages,
        dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![page.into()] },
    );
    let root = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    pdf.trailer.set("Root", root);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    bytes
}

fn chinese_pdf() -> Vec<u8> {
    use lopdf::{dictionary, Document, Object, Stream};
    let mut pdf = Document::with_version("1.5");
    let pages = pdf.new_object_id();
    let cmap = pdf.add_object(Stream::new(
        dictionary! {},
        br#"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Sample def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
5 beginbfchar
<0001> <6D77>
<0002> <6C34>
<0003> <6DE1>
<0004> <5316>
<0005> <4E00>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end"#
            .to_vec(),
    ));
    let font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "Sample",
        "Encoding" => "Identity-H",
        "ToUnicode" => cmap,
    });
    let form = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 500.into(), 500.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        },
        b"BT /F1 14 Tf 20 40 Td <0001000200030004> Tj ET".to_vec(),
    ));
    let mut kids = Vec::new();
    for content in [
        "BT /F1 14 Tf 20 40 Td <00010002> Tj 0 -14 Td <00030004> Tj ET",
        "/ChineseText Do",
        "BI /W",
    ] {
        let stream = pdf.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
        let page = pdf.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages,
            "Contents" => stream,
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font },
                "XObject" => dictionary! { "ChineseText" => form },
            },
            "MediaBox" => vec![0.into(), 0.into(), 500.into(), 500.into()],
        });
        kids.push(Object::Reference(page));
    }
    pdf.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Count" => 3, "Kids" => kids,
        }),
    );
    let root = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    pdf.trailer.set("Root", root);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    bytes
}
