//! The timeline strip: a drawn overview of the loaded timeline.
//!
//! One strip above the status text shows the whole duration with the playhead,
//! the start and end markers, and a tick at every CUE title. It is a static
//! control painted with GDI when the editor's state changes — a scrub costs
//! one repaint of a short strip and nothing else — and because it is a
//! graphic rather than text, a screen reader has nothing to announce letter
//! by letter: the timeline is read through F6 and the position reports.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rungui::{NativeHandle, Widget};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT, CreatePen,
    DeleteObject, EndPaint, FillRect, GetSysColor, GetSysColorBrush, HDC, HGDIOBJ, InvalidateRect,
    LineTo, MoveToEx, PAINTSTRUCT, PS_SOLID, SYS_COLOR_INDEX, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DefWindowProcW, GWL_STYLE, GWLP_WNDPROC, GetClientRect, GetWindowLongPtrW,
    SetWindowLongPtrW, WM_DESTROY, WM_ERASEBKGND, WM_PAINT, WS_TABSTOP,
};

type WindowProc = Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>;

/// The timeline facts the strip draws, pushed wholesale by the editor.
#[derive(Clone, Default)]
struct State {
    total: usize,
    cursor: usize,
    start: Option<usize>,
    end: Option<usize>,
    titles: Vec<usize>,
}

/// The drawn strip for one editor window.
pub struct Timeline {
    hwnd: isize,
    state: Arc<Mutex<State>>,
}

static STRIPS: OnceLock<Mutex<HashMap<isize, (Arc<Mutex<State>>, WindowProc)>>> = OnceLock::new();

fn strips() -> &'static Mutex<HashMap<isize, (Arc<Mutex<State>>, WindowProc)>> {
    STRIPS.get_or_init(|| Mutex::new(HashMap::new()))
}

impl Timeline {
    /// Turn `label` into the timeline strip: the label keeps its place in the
    /// layout, and its painting is replaced with the drawing.
    pub fn attach(label: &Widget) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let Some(NativeHandle::Win32(hwnd)) = label.native_handle() else {
            return Self { hwnd: 0, state };
        };
        let key = hwnd as isize;
        let previous_proc = unsafe {
            std::mem::transmute::<isize, WindowProc>(GetWindowLongPtrW(HWND(key), GWLP_WNDPROC))
        };
        strips()
            .lock()
            .unwrap()
            .insert(key, (state.clone(), previous_proc));
        unsafe {
            SetWindowLongPtrW(
                HWND(key),
                GWLP_WNDPROC,
                std::mem::transmute::<WindowProc, isize>(Some(wndproc)),
            );
            // The strip is a display: not a tab stop, so a screen reader's
            // focus cannot park in it during scrubs.
            let style = GetWindowLongPtrW(HWND(key), GWL_STYLE);
            SetWindowLongPtrW(HWND(key), GWL_STYLE, style & !(WS_TABSTOP.0 as isize));
        }
        Self { hwnd: key, state }
    }

    /// Push new timeline facts and repaint.
    pub fn update(
        &self,
        total: usize,
        cursor: usize,
        start: Option<usize>,
        end: Option<usize>,
        titles: Vec<usize>,
    ) {
        if self.hwnd == 0 {
            return;
        }
        {
            let mut state = self.state.lock().unwrap();
            state.total = total;
            state.cursor = cursor;
            state.start = start;
            state.end = end;
            state.titles = titles;
        }
        unsafe {
            InvalidateRect(HWND(self.hwnd), None, false);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let entry = strips().lock().unwrap().get(&hwnd.0).cloned();
        let Some((state, previous)) = entry else {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        };
        let result = match msg {
            // The strip paints itself completely; the label's own paint is
            // skipped.
            WM_PAINT => {
                draw(hwnd, &state);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            _ => CallWindowProcW(previous, hwnd, msg, wparam, lparam),
        };
        if msg == WM_DESTROY {
            strips().lock().unwrap().remove(&hwnd.0);
        }
        result
    }
}

unsafe fn draw(hwnd: HWND, state: &Arc<Mutex<State>>) {
    unsafe {
        let (total, cursor, start, end, titles) = {
            let state = state.lock().unwrap();
            (
                state.total,
                state.cursor,
                state.start,
                state.end,
                state.titles.clone(),
            )
        };
        let mut paint = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut paint);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        fill(hdc, &client, COLOR_WINDOW);
        if total > 0 {
            let width = (client.right - client.left).max(1);
            let pad = 6i32;
            let usable = (width - 2 * pad).max(1) as usize;
            let at = |frame: usize| client.left + pad + (frame.min(total) * usable / total) as i32;
            let top = client.top + 2;
            let bottom = client.bottom - 2;

            // The title ticks.
            let tick = CreatePen(PS_SOLID, 1, COLORREF(GetSysColor(COLOR_GRAYTEXT)));
            let old = SelectObject(hdc, HGDIOBJ(tick.0));
            for &frame in &titles {
                MoveToEx(hdc, at(frame), top, None);
                LineTo(hdc, at(frame), bottom);
            }
            // The start and end markers, drawn over the ticks.
            let marker = CreatePen(PS_SOLID, 2, COLORREF(GetSysColor(COLOR_WINDOWTEXT)));
            SelectObject(hdc, HGDIOBJ(marker.0));
            if let Some(start) = start {
                MoveToEx(hdc, at(start), client.top + 4, None);
                LineTo(hdc, at(start), client.bottom - 4);
            }
            if let Some(end) = end {
                MoveToEx(hdc, at(end), client.top + 4, None);
                LineTo(hdc, at(end), client.bottom - 4);
            }
            // The playhead.
            let head = CreatePen(PS_SOLID, 2, COLORREF(GetSysColor(COLOR_HIGHLIGHT)));
            SelectObject(hdc, HGDIOBJ(head.0));
            MoveToEx(hdc, at(cursor), client.top, None);
            LineTo(hdc, at(cursor), client.bottom);
            SelectObject(hdc, old);
            DeleteObject(HGDIOBJ(tick.0));
            DeleteObject(HGDIOBJ(marker.0));
            DeleteObject(HGDIOBJ(head.0));
        }
        EndPaint(hwnd, &paint);
    }
}

/// Fill with a system color such as [COLOR_WINDOW].
unsafe fn fill(hdc: HDC, rect: &RECT, color: SYS_COLOR_INDEX) {
    unsafe {
        FillRect(hdc, rect, GetSysColorBrush(color));
    }
}
