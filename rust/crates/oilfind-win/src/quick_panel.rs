//! Geometry and native presentation for the search panel.

use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, MonitorFromWindow, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::Input::Ime::{
    ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext, GCS_COMPSTR,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SystemParametersInfoW, SM_CXSCREEN, SM_CYSCREEN,
    SPI_GETHIGHCONTRAST,
};

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
            Self::Search => (860, 132),
            Self::Results => (860, 548),
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

pub(super) fn apply_material(hwnd: HWND) -> bool {
    let mut contrast = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    let high_contrast = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            (&mut contrast as *mut HIGHCONTRASTW).cast(),
            0,
        )
    } != 0
        && contrast.dwFlags & HCF_HIGHCONTRASTON != 0;
    if high_contrast {
        return false;
    }
    let set_attribute = |attribute, value: i32| unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute as u32,
            (&value as *const i32).cast(),
            std::mem::size_of::<i32>() as u32,
        )
    };
    set_attribute(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
    if set_attribute(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW) < 0 {
        return false;
    }
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    if unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) } < 0 {
        set_attribute(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_NONE);
        return false;
    }
    true
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_query_uses_the_compact_panel() {
        assert_eq!(Panel::for_query(false, " \t"), Panel::Search);
        assert_eq!(Panel::for_query(false, "文档"), Panel::Results);
        assert_eq!(Panel::for_query(true, "文档"), Panel::Settings);
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
