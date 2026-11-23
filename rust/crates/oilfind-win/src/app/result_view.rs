//! Draw the two-line body results while keeping native list-view selection.
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{HWND, LRESULT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, DrawFocusRect, DrawTextW, FillRect, GetSysColor,
    GetTextExtentPoint32W, RestoreDC, SaveDC, SelectObject, SetBkMode, SetTextColor,
    COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DT_END_ELLIPSIS,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, HDC, TRANSPARENT,
};
use windows_sys::Win32::UI::Controls::{
    ImageList_Create, ImageList_Destroy, CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDIS_FOCUS,
    CDIS_SELECTED, CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT, HIMAGELIST, ILC_COLOR32, LVIR_BOUNDS,
    LVIS_FOCUSED, LVITEMW, LVM_ENSUREVISIBLE, LVM_GETCOLUMNWIDTH, LVM_GETITEMRECT, LVM_GETTOPINDEX,
    LVM_SCROLL, LVM_SETIMAGELIST, LVSIL_SMALL, NMCUSTOMDRAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetClientRect, WM_GETFONT};

use super::{active_palette, api, quick_panel, style, WINDOW_DPI};

#[derive(Default)]
pub(super) struct RowsHeight {
    image: HIMAGELIST,
    size: i32,
}

impl RowsHeight {
    pub(super) fn sync(&mut self, list: HWND, body_mode: bool, dpi: i32) -> bool {
        let desired = if body_mode { 54 * dpi / 96 } else { 0 };
        if self.size == desired {
            TALL_ROWS.with(|tall| tall.set(desired > 0));
            return desired > 0;
        }
        let image = if desired == 0 {
            0
        } else {
            unsafe { ImageList_Create(1, desired, ILC_COLOR32, 0, 1) }
        };
        if image == 0 && desired != 0 {
            TALL_ROWS.with(|tall| tall.set(self.size > 0));
            return self.size > 0;
        }
        api::send_message(list, LVM_SETIMAGELIST, LVSIL_SMALL as usize, image as isize);
        release_image(self.image);
        self.image = image;
        self.size = desired;
        TALL_ROWS.with(|tall| tall.set(desired > 0));
        desired > 0
    }
}

impl Drop for RowsHeight {
    fn drop(&mut self) {
        release_image(self.image);
    }
}

fn release_image(image: HIMAGELIST) {
    if image != 0 {
        unsafe { ImageList_Destroy(image) };
    }
}

struct Row {
    title: Vec<u16>,
    prefix: Vec<u16>,
    matched: Vec<u16>,
    suffix: Vec<u16>,
    count: Vec<u16>,
    location: Vec<u16>,
}

thread_local! {
    static ROWS: RefCell<Vec<Row>> = const { RefCell::new(Vec::new()) };
    static TALL_ROWS: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn publish(values: &[[String; 4]], term: &str) {
    let marker = format!("【{term}】");
    ROWS.with(|rows| {
        rows.replace(
            values
                .iter()
                .map(|[title, snippet, count, location]| {
                    let (prefix, matched, suffix) = split_snippet(snippet, &marker);
                    Row {
                        title: api::wide(title),
                        prefix: api::wide(prefix),
                        matched: api::wide(matched),
                        suffix: api::wide(suffix),
                        count: api::wide(count),
                        location: api::wide(location),
                    }
                })
                .collect(),
        );
    });
}

pub(super) fn clear() {
    ROWS.with(|rows| rows.borrow_mut().clear());
}

fn split_snippet<'a>(snippet: &'a str, marker: &str) -> (&'a str, &'a str, &'a str) {
    if let Some(offset) = snippet.find(marker) {
        let open = '【'.len_utf8();
        let close = '】'.len_utf8();
        let end = offset + marker.len();
        (
            &snippet[..offset],
            &snippet[offset + open..end - close],
            &snippet[end..],
        )
    } else {
        ("", "", snippet)
    }
}

pub(super) fn custom_draw(item: &NMCUSTOMDRAW) -> Option<LRESULT> {
    if !TALL_ROWS.with(Cell::get) {
        return None;
    }
    ROWS.with(|rows| {
        let rows = rows.try_borrow().ok()?;
        if rows.is_empty() {
            return None;
        }
        match item.dwDrawStage {
            CDDS_PREPAINT => Some(CDRF_NOTIFYITEMDRAW as LRESULT),
            CDDS_ITEMPREPAINT => {
                let row = rows.get(item.dwItemSpec)?;
                draw_row(item, row).then_some(CDRF_SKIPDEFAULT as LRESULT)
            }
            _ => None,
        }
    })
}

fn draw_row(item: &NMCUSTOMDRAW, row: &Row) -> bool {
    let dc = item.hdc;
    let hwnd = item.hdr.hwndFrom;
    let mut client = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
        return false;
    }
    let bounds = RECT {
        left: client.left,
        right: client.right,
        ..item.rc
    };
    let (title_width, count_x, location_x) = {
        let width = |column| api::send_message(hwnd, LVM_GETCOLUMNWIDTH, column, 0) as i32;
        let count_x = bounds.left + width(0) + width(1);
        (width(0), count_x, count_x + width(2))
    };
    let saved = unsafe { SaveDC(dc) };
    if saved == 0 {
        return false;
    }
    let selected = item.uItemState & CDIS_SELECTED != 0;
    let high_contrast = active_palette().is_none();
    let colors = if high_contrast {
        let normal = unsafe { GetSysColor(COLOR_WINDOWTEXT) };
        let white = unsafe { GetSysColor(COLOR_WINDOW) };
        let highlight = unsafe { GetSysColor(COLOR_HIGHLIGHT) };
        let highlighted = unsafe { GetSysColor(COLOR_HIGHLIGHTTEXT) };
        if selected {
            (highlight, highlighted, highlighted, white, normal)
        } else {
            (white, normal, normal, highlight, highlighted)
        }
    } else {
        (
            if selected {
                0x00f9ebd8
            } else {
                quick_panel::CONTENT_BACKGROUND
            },
            quick_panel::CONTENT_TEXT,
            0x00634d3b,
            0x00c0eaff,
            quick_panel::CONTENT_TEXT,
        )
    };
    let (background, ink, secondary, marked, marked_ink) = colors;
    fill(dc, bounds, background);
    let dpi = WINDOW_DPI.with(|dpi| dpi.get());
    let pad = 9 * dpi / 96;
    let center = (bounds.top + bounds.bottom) / 2;
    unsafe {
        let font = api::send_message(hwnd, WM_GETFONT, 0, 0);
        if font != 0 {
            SelectObject(dc, font as _);
        }
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, ink);
    }
    let main_left = bounds.left + pad;
    let main_right = (bounds.left + title_width - pad).min(bounds.right);
    draw_text(
        dc,
        &row.title,
        main_left,
        main_right,
        bounds.top + pad / 2,
        center,
    );
    draw_text(
        dc,
        &row.count,
        count_x + pad,
        location_x - pad,
        bounds.top,
        bounds.bottom,
    );
    draw_text(
        dc,
        &row.location,
        location_x + pad,
        bounds.right - pad,
        bounds.top,
        bounds.bottom,
    );
    let snippet_bottom = bounds.bottom - pad / 2;
    let room = main_right - main_left;
    if room > 0 {
        let prefix_width = text_width(dc, &row.prefix).min(room / 3);
        let match_left = main_left + prefix_width;
        let match_width = text_width(dc, &row.matched).min(main_right - match_left);
        unsafe { SetTextColor(dc, secondary) };
        draw_text(
            dc,
            &row.prefix,
            main_left,
            match_left,
            center,
            snippet_bottom,
        );
        if match_width > 0 {
            fill(
                dc,
                RECT {
                    left: match_left,
                    right: match_left + match_width,
                    top: center + 1,
                    bottom: snippet_bottom - 1,
                },
                marked,
            );
            unsafe { SetTextColor(dc, marked_ink) };
            draw_text(
                dc,
                &row.matched,
                match_left,
                match_left + match_width,
                center,
                snippet_bottom,
            );
            unsafe { SetTextColor(dc, secondary) };
        }
        draw_text(
            dc,
            &row.suffix,
            match_left + match_width,
            main_right,
            center,
            snippet_bottom,
        );
    }
    unsafe {
        if item.uItemState & CDIS_FOCUS != 0 {
            DrawFocusRect(dc, &bounds);
        }
        RestoreDC(dc, saved);
    }
    true
}

fn draw_text(dc: HDC, value: &[u16], left: i32, right: i32, top: i32, bottom: i32) {
    if value.len() > 1 && right > left && bottom > top {
        let mut bounds = RECT {
            left,
            right,
            top,
            bottom,
        };
        unsafe {
            DrawTextW(
                dc,
                value.as_ptr(),
                -1,
                &mut bounds,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        }
    }
}

fn text_width(dc: HDC, text: &[u16]) -> i32 {
    let mut size = SIZE::default();
    if text.len() > 1 {
        unsafe { GetTextExtentPoint32W(dc, text.as_ptr(), (text.len() - 1) as i32, &mut size) };
    }
    size.cx
}

fn fill(dc: HDC, rect: RECT, color: u32) {
    let brush = unsafe { CreateSolidBrush(color) };
    if !brush.is_null() {
        unsafe {
            FillRect(dc, &rect, brush);
            DeleteObject(brush);
        }
    }
}

pub(super) fn top_index(list: HWND) -> usize {
    api::send_message(list, LVM_GETTOPINDEX, 0, 0).max(0) as usize
}

#[derive(Default)]
pub(super) struct Anchor {
    pub selected: PathBuf,
    pub top: Option<PathBuf>,
}

impl Anchor {
    pub(super) fn indices<'a>(
        &self,
        paths: impl Iterator<Item = &'a Path>,
    ) -> (Option<usize>, Option<usize>) {
        let mut selected = None;
        let mut top = None;
        for (position, path) in paths.enumerate() {
            if path == self.selected {
                selected = Some(position);
            }
            if self.top.as_deref() == Some(path) {
                top = Some(position);
            }
        }
        (selected, top)
    }
}

pub(super) fn restore(list: HWND, selected: Option<usize>, top: Option<usize>) {
    if let Some(position) = selected {
        let state = style::LVIS_SELECTED | LVIS_FOCUSED;
        let item = LVITEMW {
            state,
            stateMask: state,
            ..Default::default()
        };
        api::send_message(
            list,
            style::LVM_SETITEMSTATE,
            position,
            &item as *const LVITEMW as isize,
        );
        api::send_message(list, LVM_ENSUREVISIBLE, position, 0);
    }
    if let Some(position) = top {
        let mut bounds = RECT {
            left: LVIR_BOUNDS as i32,
            ..Default::default()
        };
        if api::send_message(
            list,
            LVM_GETITEMRECT,
            position,
            &mut bounds as *mut RECT as isize,
        ) != 0
        {
            let delta = (position as isize - top_index(list) as isize)
                * (bounds.bottom - bounds.top) as isize;
            api::send_message(list, LVM_SCROLL, 0, delta);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_highlight_retains_both_halves_of_a_chinese_sentence() {
        assert_eq!(
            split_snippet("方案采用【海水淡化】作为新增水源", "【海水淡化】"),
            ("方案采用", "海水淡化", "作为新增水源")
        );
    }

    #[test]
    fn result_anchors_follow_file_identity_after_refresh_reorders_rows() {
        let anchor = Anchor {
            selected: "report.pdf".into(),
            top: Some("memo.docx".into()),
        };
        let paths = ["new.txt", "memo.docx", "report.pdf"]
            .into_iter()
            .map(Path::new);
        assert_eq!(anchor.indices(paths), (Some(2), Some(1)));
    }
}
