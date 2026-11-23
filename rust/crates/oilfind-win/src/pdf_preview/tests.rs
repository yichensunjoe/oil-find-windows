use lopdf::{dictionary, Document, Object, Stream};

use super::*;

fn sample_pdf(pages: &[(&str, i32, [i32; 4])]) -> Arc<Vec<u8>> {
    save_pdf(sample_document(pages).0)
}

fn sample_document(pages: &[(&str, i32, [i32; 4])]) -> (Document, lopdf::ObjectId) {
    let mut pdf = Document::with_version("1.5");
    let parent = pdf.new_object_id();
    let font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let mut children = Vec::new();
    for &(content, rotation, crop) in pages {
        let stream = pdf.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
        let page = pdf.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => parent,
            "Contents" => stream,
            "MediaBox" => vec![0.into(), 0.into(), 240.into(), 160.into()],
            "CropBox" => crop.map(Object::from).to_vec(),
            "Rotate" => rotation,
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        });
        children.push(Object::Reference(page));
    }
    pdf.objects.insert(
        parent,
        dictionary! { "Type" => "Pages", "Count" => children.len() as i64, "Kids" => children }
            .into(),
    );
    let root = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => parent });
    pdf.trailer.set("Root", root);
    (pdf, font)
}

fn save_pdf(mut pdf: Document) -> Arc<Vec<u8>> {
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    Arc::new(bytes)
}

fn pixel(page: &RenderedPage, x: usize, y: usize) -> &[u8] {
    let offset = (y * page.width as usize + x) * 4;
    &page.bgra[offset..offset + 4]
}

#[test]
fn original_page_raster_highlights_only_the_selected_occurrence() {
    let pdf = sample_pdf(&[
        ("1 0 0 rg 0 0 240 160 re f", 0, [0, 0, 240, 160]),
        (
            "0 0 1 rg 27 59 25 15 re f 0 g \
             BT /F1 15 Tf 19 128 Td (needle) Tj ET \
             BT /F1 15 Tf 80 44 Td (needle) Tj ET",
            0,
            [0, 0, 240, 160],
        ),
    ]);
    let rendered = render(pdf.clone(), 2, "needle\nneedle\n", 7..13).unwrap();
    let unverified = render(pdf, 2, "needle\nneedle\nextra", 7..13).unwrap();
    let highlight = rendered.highlight.unwrap();
    assert_eq!(unverified.highlight, None);
    assert_eq!((rendered.width, rendered.height), (480, 320));
    assert_eq!(pixel(&rendered, 64, 188), &[255, 0, 0, 255]);
    assert!(highlight.left >= 156 && highlight.right <= 260);
    assert!(highlight.top >= 206 && highlight.bottom <= 240);
    let mut changed_pixels = 0;
    for (index, (highlighted, plain)) in rendered
        .bgra
        .chunks_exact(4)
        .zip(unverified.bgra.chunks_exact(4))
        .enumerate()
    {
        if highlighted != plain {
            let x = index as u32 % rendered.width;
            let y = index as u32 / rendered.width;
            assert!((highlight.left..highlight.right).contains(&x));
            assert!((highlight.top..highlight.bottom).contains(&y));
            changed_pixels += 1;
        }
    }
    assert!(changed_pixels > 100);
}

#[test]
fn an_unreadable_graphics_stream_rejects_the_whole_original_page() {
    let (mut pdf, _) =
        sample_document(&[("BT /F1 15 Tf 19 128 Td (needle) Tj ET", 0, [0, 0, 240, 160])]);
    let page = pdf.page_iter().next().unwrap();
    let graphics = pdf.add_object(Stream::new(
        dictionary! {},
        b"0 0 1 rg 27 59 25 15 re f".to_vec(),
    ));
    let dictionary = pdf.get_object_mut(page).unwrap().as_dict_mut().unwrap();
    let text = dictionary.get(b"Contents").unwrap().clone();
    dictionary.set("Contents", vec![text, Object::Reference(graphics)]);

    let original = render(save_pdf(pdf.clone()), 1, "needle\n", 0..6).unwrap();
    assert_eq!(pixel(&original, 64, 188), &[255, 0, 0, 255]);
    assert!(original.highlight.is_some());

    pdf.get_object_mut(graphics)
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .dict
        .set("Filter", Object::Name(b"DCTDecode".to_vec()));
    assert!(render(save_pdf(pdf), 1, "needle\n", 0..6).is_err());
}

#[test]
fn a_dropped_page_tree_entry_cannot_redirect_to_another_page_with_the_same_hit() {
    let (mut pdf, _) = sample_document(&[
        ("BT /F1 15 Tf 19 128 Td (needle) Tj ET", 0, [0, 0, 240, 160]),
        ("BT /F1 15 Tf 80 44 Td (needle) Tj ET", 0, [0, 0, 240, 160]),
    ]);
    let first = pdf.page_iter().next().unwrap();
    let parent = pdf
        .get_dictionary(first)
        .unwrap()
        .get(b"Parent")
        .unwrap()
        .as_reference()
        .unwrap();
    let malformed_kid = pdf.add_object(Object::Integer(0));
    let parent = pdf.get_object_mut(parent).unwrap().as_dict_mut().unwrap();
    let kids = parent.get_mut(b"Kids").unwrap().as_array_mut().unwrap();
    kids.insert(0, Object::Reference(malformed_kid));
    parent.set("Count", 3);
    assert_eq!(pdf.page_iter().nth(1), Some(first));
    assert!(render(save_pdf(pdf), 2, "needle\n", 0..6).is_err());
}

#[test]
fn crop_and_rotation_apply_to_the_bitmap_and_multibyte_selection() {
    let pdf = sample_pdf(&[(
        r"BT /F1 16 Tf 30 38 Td (caf\351) Tj ET",
        90,
        [10, 20, 190, 130],
    )]);
    let rendered = render(pdf, 1, "café\n", 3..5).unwrap();
    let highlight = rendered.highlight.unwrap();
    assert_eq!((rendered.width, rendered.height), (220, 360));
    assert!(highlight.left >= 32 && highlight.right <= 68);
    assert!(highlight.top >= 78 && highlight.bottom <= 114);
    assert!(rendered.bgra.chunks_exact(4).any(|pixel| pixel[2] < 30));
}

#[test]
fn a_hidden_or_clipped_match_returns_the_page_without_a_highlight() {
    let pdf = sample_pdf(&[(
        "BT /F1 15 Tf 19 128 Td (visible) Tj ET \
         BT /F1 15 Tf 3 Tr 20 90 Td (hidden) Tj ET \
         q 0 0 10 10 re W n BT /F1 15 Tf 0 Tr 20 50 Td (clipped) Tj ET Q",
        0,
        [0, 0, 240, 160],
    )]);
    let page_text = "visible\nhidden\nclipped\n";
    let hidden = render(pdf.clone(), 1, page_text, 8..14).unwrap();
    let clipped = render(pdf.clone(), 1, page_text, 15..22).unwrap();
    let visible = render(pdf, 1, page_text, 0..7).unwrap();
    assert_eq!(hidden.highlight, None);
    assert_eq!(clipped.highlight, None);
    assert!(visible.highlight.is_some());
    assert_eq!(hidden.bgra, clipped.bgra);
    assert!(hidden.bgra.chunks_exact(4).any(|pixel| pixel[2] < 30));
}

#[test]
fn rejects_invalid_pages_and_partial_utf8_byte_ranges() {
    let pdf = sample_pdf(&[("", 0, [0, 0, 240, 160])]);
    assert!(render(pdf.clone(), 0, "café", 0..5).is_err());
    assert!(render(pdf.clone(), 2, "café", 0..5).is_err());
    assert!(render(pdf, 1, "café", 4..5).is_err());
    assert!(render(Arc::new(b"invalid".to_vec()), 1, "x", 0..1).is_err());
}

#[test]
fn a_page_with_no_visible_crop_area_cannot_be_highlighted() {
    let pdf = sample_pdf(&[("BT /F1 15 Tf 19 128 Td (hidden) Tj ET", 0, [0; 4])]);
    assert!(render(pdf, 1, "hidden\n", 0..6).is_err());
}

#[test]
fn unavailable_cid_font_does_not_substitute_unrelated_glyphs() {
    let (mut pdf, font) =
        sample_document(&[("BT /F1 20 Tf 30 120 Td <0024> Tj ET", 0, [0, 0, 240, 160])]);
    let cid_font = pdf.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "UnavailableFont",
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "CIDToGIDMap" => "Identity",
    });
    let to_unicode = pdf.add_object(Stream::new(
        dictionary! {},
        b"begincmap\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\n\
          endcodespacerange\n1 beginbfchar\n<0024> <6D77>\nendbfchar\nendcmap"
            .to_vec(),
    ));
    pdf.objects.insert(
        font,
        dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "UnavailableFont",
            "Encoding" => "Identity-H",
            "DescendantFonts" => vec![Object::Reference(cid_font)],
            "ToUnicode" => to_unicode,
        }
        .into(),
    );
    assert!(render(save_pdf(pdf), 1, "海\n", 0..3).is_err());
}
