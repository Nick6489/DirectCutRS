//! Native keyboard handling for read-only edit fields and push buttons.
//!
//! A multi-line EDIT control answers `WM_GETDLGCODE` by claiming the Tab key
//! for itself, so once keyboard focus lands in one, Tab stops cycling: it
//! would insert a tab character, and in a read-only field it does nothing at
//! all. The stream archiver's title log was the reported trap: after one full
//! Tab pass through the window, focus reached the log and stayed there.
//!
//! Every multi-line text view in this application is read-only, so Tab can
//! only ever mean "move on". [`allow_tab_to_escape`] subclasses the control
//! and answers `WM_GETDLGCODE` for Tab itself, handing the key back to the
//! dialog manager that drives rungui's focus cycling, and forwards everything
//! else to the toolkit's own procedure.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use rungui::{NativeHandle, TextArea};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DLGC_WANTCHARS, DefWindowProcW, GWLP_WNDPROC, GetWindowLongPtrW,
    SetWindowLongPtrW, WM_DESTROY, WM_GETDLGCODE,
};

/// The virtual-key code rungui's dialog manager queries when Tab is pressed.
const VK_TAB: usize = 0x0009;

type WindowProc = Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>;

static SUBCLASSED: OnceLock<Mutex<HashMap<isize, WindowProc>>> = OnceLock::new();

fn subclassed() -> &'static Mutex<HashMap<isize, WindowProc>> {
    SUBCLASSED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Make Tab and Shift+Tab move keyboard focus out of `area`.
pub fn allow_tab_to_escape(area: &TextArea) {
    let Some(NativeHandle::Win32(hwnd)) = area.native_handle() else {
        return;
    };
    let key = hwnd as isize;
    let mut subclassed = subclassed().lock().unwrap();
    if subclassed.contains_key(&key) {
        return;
    }
    let h = HWND(key);
    let previous = unsafe { GetWindowLongPtrW(h, GWLP_WNDPROC) };
    subclassed.insert(key, unsafe {
        std::mem::transmute::<isize, WindowProc>(previous)
    });
    drop(subclassed);
    unsafe {
        SetWindowLongPtrW(
            h,
            GWLP_WNDPROC,
            std::mem::transmute::<WindowProc, isize>(Some(wndproc)),
        );
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let key = hwnd.0;
    let previous = subclassed().lock().unwrap().get(&key).copied();
    // Everything is forwarded to the toolkit's procedure, which runs the
    // control's own handling; only the Tab query is answered here.
    let result = match previous {
        Some(previous) if msg != WM_GETDLGCODE || wparam.0 != VK_TAB => unsafe {
            CallWindowProcW(previous, hwnd, msg, wparam, lparam)
        },
        Some(_) => LRESULT(DLGC_WANTCHARS as isize),
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    if msg == WM_DESTROY {
        // The toolkit's procedure ran its own cleanup above; stop holding the
        // chain so the map cannot grow on every recreated field.
        subclassed().lock().unwrap().remove(&key);
    }
    result
}

/// Let Enter activate a focused button in a Rungui top-level window.
#[cfg(windows)]
pub fn activate_on_enter(button: &rungui::Button) {
    use windows::Win32::UI::Shell::SetWindowSubclass;

    if let Some(NativeHandle::Win32(hwnd)) = button.native_handle() {
        unsafe { SetWindowSubclass(HWND(hwnd as isize), Some(button_proc), 0, 0) };
    }
}

#[cfg(windows)]
unsafe extern "system" fn button_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        BM_CLICK, DLGC_WANTMESSAGE, MSG, SendMessageW, WM_CHAR, WM_KEYDOWN, WM_NCDESTROY,
    };

    const VK_RETURN: usize = 0x0d;

    match msg {
        WM_GETDLGCODE if wparam.0 == VK_RETURN && lparam.0 != 0 => {
            let key = unsafe { &*(lparam.0 as *const MSG) };
            if key.message == WM_KEYDOWN || key.message == WM_CHAR {
                // Rungui calls IsDialogMessage on its top-level window.
                // Claim Enter before that call can consume it.
                let flags = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
                return LRESULT(flags.0 | DLGC_WANTMESSAGE as isize);
            }
        }
        WM_KEYDOWN if wparam.0 == VK_RETURN => {
            if lparam.0 & (1 << 30) == 0 {
                unsafe { SendMessageW(hwnd, BM_CLICK, WPARAM(0), LPARAM(0)) };
            }
            return LRESULT(0);
        }
        WM_CHAR if wparam.0 == VK_RETURN => return LRESULT(0),
        WM_NCDESTROY => {
            unsafe { RemoveWindowSubclass(hwnd, Some(button_proc), id) };
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::allow_tab_to_escape;
    use rungui::{TextArea, WidgetId};

    #[test]
    fn styling_a_dead_field_does_nothing() {
        allow_tab_to_escape(&TextArea::from_id(WidgetId::DEAD));
    }
}
