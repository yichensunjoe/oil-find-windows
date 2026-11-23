//! Scrollable Win32 surface for a rendered, highlighted PDF page.
use std::cell::RefCell;
use std::ptr::null_mut;
use std::sync::Arc;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, FillRect, GetSysColorBrush, InvalidateRect, SetStretchBltMode,
    StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, COLOR_APPWORKSPACE, DIB_RGB_COLORS,
    HALFTONE, PAINTSTRUCT, SRCCOPY,
};
use windows_sys::Win32::UI::Controls::SetScrollInfo;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, VK_DOWN, VK_END, VK_HOME, VK_NEXT, VK_PRIOR, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetClientRect, GetScrollInfo, LoadCursorW, RegisterClassExW, DLGC_WANTARROWS,
    IDC_ARROW, SB_BOTTOM, SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBPOSITION,
    SB_THUMBTRACK, SB_TOP, SB_VERT, SCROLLINFO, SIF_DISABLENOSCROLL, SIF_PAGE, SIF_POS, SIF_RANGE,
    SIF_TRACKPOS, WHEEL_DELTA, WM_ERASEBKGND, WM_GETDLGCODE, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_MOUSEWHEEL, WM_NCDESTROY, WM_PAINT, WM_SIZE, WM_VSCROLL, WNDCLASSEXW, WS_BORDER, WS_TABSTOP,
    WS_VSCROLL,
};

use super::{api, WINDOW_DPI};
use crate::pdf_preview::RenderedPage;

const CLASS: &str = "OilFindPdfPage";

#[derive(Default)]
struct Surface {
    page: Option<Arc<RenderedPage>>,
    scroll: i32,
}

thread_local! {
    static SURFACE: RefCell<Surface> = const { RefCell::new(Surface { page: None, scroll: 0 }) };
}

pub(super) fn create(parent: HWND, instance: HINSTANCE) -> Result<HWND, String> {
    let name = api::wide(CLASS);
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(canvas_proc),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        lpszClassName: name.as_ptr(),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    api::create_control(
        parent,
        instance,
        CLASS,
        "PDF 原页预览",
        WS_TABSTOP | WS_BORDER | WS_VSCROLL,
        0,
        167,
        null_mut(),
    )
}

#[derive(Debug)]
struct Geometry {
    width: i32,
    height: i32,
    x: i32,
    margin: i32,
    viewport_height: i32,
}

impl Geometry {
    fn fit(client_width: i32, client_height: i32, width: u32, height: u32, margin: i32) -> Self {
        let scaled_width = (client_width - 2 * margin).max(1).min(width as i32);
        let scaled_height =
            (i64::from(height) * i64::from(scaled_width) / i64::from(width.max(1))) as i32;
        Self {
            width: scaled_width,
            height: scaled_height.max(1),
            x: (client_width - scaled_width) / 2,
            margin,
            viewport_height: client_height.max(1),
        }
    }

    fn scroll_limit(&self) -> i32 {
        (self.height + 2 * self.margin - self.viewport_height).max(0)
    }

    fn scroll_to_hit(&self, y: u32, original_height: u32) -> i32 {
        let offset = i64::from(y) * i64::from(self.height) / i64::from(original_height.max(1));
        (offset as i32 + self.margin - self.viewport_height / 2).clamp(0, self.scroll_limit())
    }
}

fn client(hwnd: HWND) -> RECT {
    let mut bounds = RECT::default();
    unsafe { GetClientRect(hwnd, &mut bounds) };
    bounds
}

fn geometry(bounds: RECT, page: &RenderedPage) -> Geometry {
    let margin = WINDOW_DPI.with(|dpi| 12 * dpi.get() / 96);
    Geometry::fit(bounds.right, bounds.bottom, page.width, page.height, margin)
}

pub(super) fn set_page(hwnd: HWND, page: Option<Arc<RenderedPage>>) {
    if hwnd.is_null() {
        return;
    }
    let offset = page
        .as_ref()
        .and_then(|page| {
            let hit = page.highlight?;
            let layout = geometry(client(hwnd), page);
            Some(layout.scroll_to_hit((hit.top + hit.bottom) / 2, page.height))
        })
        .unwrap_or(0);
    SURFACE.with(|state| {
        *state.borrow_mut() = Surface {
            page,
            scroll: offset,
        }
    });
    scroll(hwnd, offset);
}

fn scroll(hwnd: HWND, position: i32) {
    let bounds = client(hwnd);
    let (position, extent) = SURFACE.with(|surface| {
        let Ok(mut state) = surface.try_borrow_mut() else {
            return (0, 0);
        };
        let extent = state
            .page
            .as_ref()
            .map_or(0, |page| geometry(bounds, page).scroll_limit());
        state.scroll = position.clamp(0, extent);
        (state.scroll, extent)
    });
    let height = bounds.bottom.max(1);
    let info = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS | SIF_DISABLENOSCROLL,
        nMin: 0,
        nMax: extent + height - 1,
        nPage: height as u32,
        nPos: position,
        ..Default::default()
    };
    unsafe {
        SetScrollInfo(hwnd, SB_VERT, &info, 1);
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}

fn scroll_offset() -> i32 {
    SURFACE.with(|surface| surface.try_borrow().map_or(0, |surface| surface.scroll))
}

fn scroll_by_command(hwnd: HWND, command: i32) {
    let position = scroll_offset();
    let line = WINDOW_DPI.with(|dpi| 28 * dpi.get() / 96);
    let page = (client(hwnd).bottom - line).max(line);
    let target = match command {
        SB_LINEUP => position - line,
        SB_LINEDOWN => position + line,
        SB_PAGEUP => position - page,
        SB_PAGEDOWN => position + page,
        SB_TOP => 0,
        SB_BOTTOM => i32::MAX,
        SB_THUMBPOSITION | SB_THUMBTRACK => {
            let mut info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_TRACKPOS,
                ..Default::default()
            };
            if unsafe { GetScrollInfo(hwnd, SB_VERT, &mut info) } == 0 {
                return;
            }
            info.nTrackPos
        }
        _ => return,
    };
    scroll(hwnd, target);
}

fn paint(hwnd: HWND) {
    let mut paint = PAINTSTRUCT::default();
    let dc = unsafe { BeginPaint(hwnd, &mut paint) };
    if !dc.is_null() {
        let bounds = client(hwnd);
        unsafe { FillRect(dc, &bounds, GetSysColorBrush(COLOR_APPWORKSPACE)) };
        SURFACE.with(|surface| {
            let Ok(surface) = surface.try_borrow() else {
                return;
            };
            let Some(page) = &surface.page else { return };
            let layout = geometry(bounds, page);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: page.width as i32,
                    biHeight: -(page.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            unsafe {
                SetStretchBltMode(dc, HALFTONE);
                StretchDIBits(
                    dc,
                    layout.x,
                    layout.margin - surface.scroll,
                    layout.width,
                    layout.height,
                    0,
                    0,
                    page.width as i32,
                    page.height as i32,
                    page.bgra.as_ptr().cast(),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            }
        });
    }
    unsafe { EndPaint(hwnd, &paint) };
}

unsafe extern "system" fn canvas_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_GETDLGCODE => DLGC_WANTARROWS as LRESULT,
        WM_SIZE => {
            scroll(hwnd, scroll_offset());
            0
        }
        WM_VSCROLL => {
            scroll_by_command(hwnd, (wparam & 0xffff) as i32);
            0
        }
        WM_MOUSEWHEEL => {
            let delta = (wparam >> 16) as i16 as i32;
            let line = WINDOW_DPI.with(|dpi| 28 * dpi.get() / 96);
            scroll(
                hwnd,
                scroll_offset() - delta * 3 * line / WHEEL_DELTA as i32,
            );
            0
        }
        WM_LBUTTONDOWN => {
            unsafe { SetFocus(hwnd) };
            0
        }
        WM_KEYDOWN => {
            let command = match wparam as u16 {
                VK_UP => SB_LINEUP,
                VK_DOWN => SB_LINEDOWN,
                VK_PRIOR => SB_PAGEUP,
                VK_NEXT => SB_PAGEDOWN,
                VK_HOME => SB_TOP,
                VK_END => SB_BOTTOM,
                _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
            };
            scroll_by_command(hwnd, command);
            0
        }
        WM_NCDESTROY => {
            SURFACE.with(|state| {
                if let Ok(mut state) = state.try_borrow_mut() {
                    *state = Surface::default();
                }
            });
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hit_below_the_fold_is_visible_when_the_pdf_canvas_opens() {
        let canvas = Geometry::fit(800, 400, 1000, 1500, 12);
        let offset = canvas.scroll_to_hit(1050, 1500);
        assert_eq!(canvas.width, 776);
        assert_eq!(canvas.height, 1164);
        assert_eq!(offset, 626);
        assert_eq!(canvas.scroll_to_hit(1499, 1500), canvas.scroll_limit());
    }
}
