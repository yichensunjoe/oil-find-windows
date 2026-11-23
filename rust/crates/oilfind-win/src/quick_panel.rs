//! Geometry and native presentation for the search panel.

use windows_sys::Win32::Foundation::{HWND, LRESULT, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows_sys::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawFocusRect, DrawTextW, FillRect,
    FillRgn, GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, MonitorFromWindow, RestoreDC,
    SaveDC, SelectObject, SetBkColor, SetBkMode, SetTextColor, DT_CENTER, DT_END_ELLIPSIS,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, HBRUSH, HDC, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, OPAQUE, TRANSPARENT,
};
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows_sys::Win32::UI::Controls::{CDIS_FOCUS, CDIS_SELECTED, NMCUSTOMDRAW};
use windows_sys::Win32::UI::Input::Ime::{
    ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext, GCS_COMPSTR,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SendMessageW, SystemParametersInfoW, SM_CXSCREEN, SM_CYSCREEN,
    SPI_GETHIGHCONTRAST, WM_GETFONT,
};

use crate::api;

const fn rgb(red: u32, green: u32, blue: u32) -> u32 {
    red | (green << 8) | (blue << 16)
}

const PANEL_BACKGROUND: u32 = rgb(243, 246, 251);
pub(super) const CONTENT_BACKGROUND: u32 = rgb(255, 255, 255);
pub(super) const CONTENT_TEXT: u32 = rgb(24, 38, 55);
const SECONDARY_TEXT: u32 = rgb(59, 77, 99);
const FRAME_BORDER: u32 = rgb(207, 219, 234);
const BUTTON_FACE: u32 = rgb(228, 236, 248);
const BUTTON_PRESSED: u32 = rgb(203, 220, 244);

#[derive(Clone, Copy)]
pub(super) struct Palette {
    panel: HBRUSH,
    content: HBRUSH,
}

impl Palette {
    pub(super) fn new() -> Option<Self> {
        let palette = unsafe {
            Self {
                panel: CreateSolidBrush(PANEL_BACKGROUND),
                content: CreateSolidBrush(CONTENT_BACKGROUND),
            }
        };
        if palette.panel.is_null() || palette.content.is_null() {
            palette.release();
            return None;
        }
        Some(palette)
    }

    pub(super) fn release(self) {
        unsafe {
            if !self.panel.is_null() {
                DeleteObject(self.panel);
            }
            if !self.content.is_null() {
                DeleteObject(self.content);
            }
        }
    }

    pub(super) fn erase_panel(self, dc: HDC, bounds: &RECT) {
        unsafe { FillRect(dc, bounds, self.panel) };
    }

    pub(super) fn control_colors(self, dc: HDC, on_content: bool, muted: bool) -> LRESULT {
        let (brush, background) = if on_content {
            (self.content, CONTENT_BACKGROUND)
        } else {
            (self.panel, PANEL_BACKGROUND)
        };
        unsafe {
            SetBkMode(dc, OPAQUE as i32);
            SetBkColor(dc, background);
            SetTextColor(dc, if muted { SECONDARY_TEXT } else { CONTENT_TEXT });
        }
        brush as LRESULT
    }

    pub(super) fn draw_header_button(self, item: &NMCUSTOMDRAW, dpi: i32) -> bool {
        let dc = item.hdc;
        let bounds = item.rc;
        let saved = unsafe { SaveDC(dc) };
        if saved == 0 {
            return false;
        }
        self.erase_panel(dc, &bounds);
        let radius = (12 * dpi / 96).max(1);
        unsafe {
            let region = CreateRoundRectRgn(
                bounds.left,
                bounds.top,
                bounds.right,
                bounds.bottom,
                radius,
                radius,
            );
            let fill = CreateSolidBrush(if item.uItemState & CDIS_SELECTED != 0 {
                BUTTON_PRESSED
            } else {
                BUTTON_FACE
            });
            if !region.is_null() && !fill.is_null() {
                FillRgn(dc, region, fill);
            }
            if !region.is_null() {
                DeleteObject(region);
            }
            if !fill.is_null() {
                DeleteObject(fill);
            }
            let font = SendMessageW(item.hdr.hwndFrom, WM_GETFONT, 0, 0);
            if font != 0 {
                SelectObject(dc, font as _);
            }
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, CONTENT_TEXT);
            let label = api::wide(&api::text(item.hdr.hwndFrom));
            let inset = (6 * dpi / 96).max(1);
            let mut text_rect = RECT {
                left: bounds.left + inset,
                right: bounds.right - inset,
                ..bounds
            };
            DrawTextW(
                dc,
                label.as_ptr(),
                -1,
                &mut text_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            if item.uItemState & CDIS_FOCUS != 0 {
                let focus_rect = RECT {
                    left: bounds.left + 3,
                    top: bounds.top + 3,
                    right: bounds.right - 3,
                    bottom: bounds.bottom - 3,
                };
                DrawFocusRect(dc, &focus_rect);
            }
            RestoreDC(dc, saved);
        }
        true
    }
}

pub(super) fn uses_system_colors() -> bool {
    let mut contrast = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            (&mut contrast as *mut HIGHCONTRASTW).cast(),
            0,
        )
    };
    read == 0 || contrast.dwFlags & HCF_HIGHCONTRASTON != 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Panel {
    Search,
    Results,
    Settings,
}

impl Panel {
    pub(super) fn for_query(settings: bool, query: &str) -> Self {
        if settings {
            Self::Settings
        } else if query.trim().is_empty() {
            Self::Search
        } else {
            Self::Results
        }
    }

    pub(super) fn bounds(self, work: RECT, dpi: i32) -> RECT {
        let scale = |value| value * dpi / 96;
        let (desired_width, desired_height) = match self {
            Self::Search => (860, 151),
            Self::Results => (860, 562),
            Self::Settings => (860, 422),
        };
        let work_width = (work.right - work.left).max(1);
        let work_height = (work.bottom - work.top).max(1);
        let width = scale(desired_width).min((work_width - scale(32)).max(1));
        let height = scale(desired_height).min((work_height - scale(32)).max(1));
        let left = work.left + (work_width - width) / 2;
        // Keep the search field at the same height while the results expand.
        let top = work.top + (work_height / 5).min((work_height - height) / 2);
        RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }
}

pub(super) fn work_area(hwnd: HWND, follow_pointer: bool) -> RECT {
    let mut point = POINT::default();
    let monitor = unsafe {
        if follow_pointer && GetCursorPos(&mut point) != 0 {
            MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST)
        } else {
            MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)
        }
    };
    monitor_work_area(monitor)
}

pub(super) fn work_area_near(rect: &RECT) -> RECT {
    monitor_work_area(unsafe { MonitorFromRect(rect, MONITOR_DEFAULTTONEAREST) })
}

fn monitor_work_area(monitor: HMONITOR) -> RECT {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !monitor.is_null() && unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
        return info.rcWork;
    }
    unsafe {
        RECT {
            left: 0,
            top: 0,
            right: GetSystemMetrics(SM_CXSCREEN),
            bottom: GetSystemMetrics(SM_CYSCREEN),
        }
    }
}

pub(super) fn apply_frame(hwnd: HWND, custom_colors: bool) {
    // DWM styles the outer frame. GDI draws all content onto opaque surfaces.
    let set_attribute = |attribute, value: u32| unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute as u32,
            (&value as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    set_attribute(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND as u32);
    let border = if custom_colors {
        FRAME_BORDER
    } else {
        u32::MAX
    };
    set_attribute(DWMWA_BORDER_COLOR, border);
}

pub(super) fn composing(edit: HWND) -> bool {
    unsafe {
        let context = ImmGetContext(edit);
        if context.is_null() {
            return false;
        }
        let size = ImmGetCompositionStringW(context, GCS_COMPSTR, std::ptr::null_mut(), 0);
        ImmReleaseContext(edit, context);
        size > 0
    }
}

pub(super) fn result_columns(width: i32, dpi: i32) -> [i32; 4] {
    let px = |value| value * dpi / 96;
    let available = (width - px(20) - 2).max(4);
    let name = px(218).min(available * 30 / 100).max(1);
    let size = px(93).min(available * 13 / 100).max(1);
    let date = px(183).min(available * 25 / 100).max(1);
    [name, available - name - size - date, size, date]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: u32) -> f64 {
        let linear = |shift: u32| {
            let component = f64::from((color >> shift) & 0xffu32) / 255.0;
            if component <= 0.04045 {
                component / 12.92
            } else {
                ((component + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(0) + 0.7152 * linear(8) + 0.0722 * linear(16)
    }

    #[test]
    fn text_contrast_is_high_on_all_opaque_surfaces() {
        for (text, background) in [
            (CONTENT_TEXT, CONTENT_BACKGROUND),
            (CONTENT_TEXT, PANEL_BACKGROUND),
            (SECONDARY_TEXT, PANEL_BACKGROUND),
            (CONTENT_TEXT, BUTTON_FACE),
            (CONTENT_TEXT, BUTTON_PRESSED),
        ] {
            let contrast = (luminance(background) + 0.05) / (luminance(text) + 0.05);
            assert!(contrast >= 7.0, "text contrast is only {contrast:.1}:1");
        }
    }

    #[test]
    fn blank_query_uses_the_compact_panel() {
        assert_eq!(Panel::for_query(false, " \t"), Panel::Search);
        assert_eq!(Panel::for_query(false, "文档"), Panel::Results);
        assert_eq!(Panel::for_query(true, "文档"), Panel::Settings);
    }

    #[test]
    fn result_columns_leave_room_for_the_scrollbar() {
        for (width, dpi) in [(816, 96), (1224, 144), (700, 192)] {
            let columns = result_columns(width, dpi);
            assert!(columns.iter().all(|&column| column > 0));
            assert!(
                columns.iter().sum::<i32>() <= width - 20 * dpi / 96,
                "columns should fit beside the vertical scrollbar: {columns:?}"
            );
        }
    }

    #[test]
    fn panels_fit_high_dpi_monitors_with_negative_origins() {
        let work = RECT {
            left: -1024,
            top: -240,
            right: 0,
            bottom: 488,
        };
        for panel in [Panel::Search, Panel::Results, Panel::Settings] {
            let bounds = panel.bounds(work, 192);
            assert!(bounds.left >= work.left && bounds.right <= work.right);
            assert!(bounds.top >= work.top && bounds.bottom <= work.bottom);
        }
    }
}
