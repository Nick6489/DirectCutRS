//! Keyboard focus around modal dialogs.
//!
//! When a modal dialog over a window is dismissed, Windows re-activates the
//! window but does not reliably hand keyboard focus back to the control that
//! had it: focus can stay with the dialog's field as it dies, or land on
//! whatever the dialog manager picks, and a screen reader then treats that
//! landing spot as its context. The reported form of this was the editor's
//! status line reading itself letter by letter on every arrow key after the
//! cue point prompt closed, where before the dialog the same keys scrubbed.
//!
//! Every dialog therefore runs between [`capture`] and [`restore`]: the
//! control that holds keyboard focus is remembered before the dialog opens
//! and asked for again afterwards — immediately, and twice more from the
//! event loop, because the activation handoff after a modal is asynchronous
//! and can pre-empt a single restore.
//!
//! When nothing in the window held focus — the normal case for a dialog
//! opened from a menu command, where the menu still owns activation — the
//! fallback is the window itself. The editor is designed around that state:
//! its keys are menu accelerators, which work whatever holds focus, and a
//! window-level focus gives the screen reader a quiet object instead of a
//! text field whose text changes on every scrub.

use rungui::{NativeHandle, Timer, Widget, Window};

#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, IsWindow};

// GetFocus and SetFocus live in the KeyboardAndMouse module of the windows
// crate, which this build does not enable as a feature. The stream capture
// window's focus tooling declares them the same way.
#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetFocus() -> isize;
    fn GetForegroundWindow() -> isize;
    fn SetFocus(hwnd: isize) -> isize;
}

/// A control of the window a dialog will be shown over, which holds
/// keyboard focus right now.
#[derive(Clone, Copy)]
pub struct Focus {
    hwnd: isize,
}

impl Focus {
    /// Delayed restores must not steal focus from a newly opened window.
    fn apply_if_active(&self) {
        #[cfg(windows)]
        unsafe {
            if IsWindow(HWND(self.hwnd)).as_bool()
                && GetAncestor(HWND(self.hwnd), GA_ROOT).0 == GetForegroundWindow()
                && GetFocus() == 0
            {
                self.apply();
            }
        }
        #[cfg(not(windows))]
        self.apply();
    }

    /// Put keyboard focus back on the remembered control. A control that
    /// has since been destroyed is skipped.
    fn apply(&self) {
        #[cfg(windows)]
        unsafe {
            if IsWindow(HWND(self.hwnd)).as_bool() {
                SetFocus(self.hwnd);
            }
        }
        #[cfg(not(windows))]
        let _ = self.hwnd;
    }
}

/// The focused control of the window `parent` belongs to, when keyboard
/// focus is inside it right now. `None` otherwise: focus in another
/// top-level window, in a menu, or nowhere. Capture runs before the dialog
/// opens.
#[cfg(windows)]
pub fn capture(parent: Window) -> Option<Focus> {
    let root = match parent.native_handle() {
        Some(NativeHandle::Win32(hwnd)) => hwnd as isize,
        _ => return None,
    };
    let focus = unsafe { GetFocus() };
    if focus == 0 {
        return None;
    }
    let focus_root = unsafe { GetAncestor(HWND(focus), GA_ROOT) }.0;
    (focus_root == root).then_some(Focus { hwnd: focus })
}

#[cfg(not(windows))]
pub fn capture(_parent: Window) -> Option<Focus> {
    None
}

pub fn has_focus(target: &Widget) -> bool {
    #[cfg(windows)]
    {
        matches!(target.native_handle(), Some(NativeHandle::Win32(hwnd)) if unsafe { GetFocus() } == hwnd as isize)
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        false
    }
}

/// Re-establish keyboard focus after a modal dialog closes: the remembered
/// control when one was captured, otherwise `fallback`. The restore is
/// repeated from the event loop because Windows completes its activation
/// handoff after the dialog's own teardown.
pub fn restore(focus: Option<Focus>, fallback: &Widget) {
    match focus {
        Some(focus) => {
            focus.apply();
            for delay in [0, 100] {
                Timer::once(delay, move || focus.apply_if_active());
            }
        }
        None => settle(fallback),
    }
}

/// Put keyboard focus on `target` now and again from the event loop, for
/// the same asynchronous handoff. Used when a window closes or is shown
/// and its owner is meant to have focus back.
pub fn settle(target: &Widget) {
    let target = *target;
    target.focus();
    for delay in [0, 100] {
        Timer::once(delay, move || {
            if target.is_alive() {
                #[cfg(windows)]
                if let Some(NativeHandle::Win32(hwnd)) = target.native_handle() {
                    unsafe {
                        if GetAncestor(HWND(hwnd as isize), GA_ROOT).0 == GetForegroundWindow()
                            && GetFocus() == 0
                        {
                            target.focus();
                        }
                    }
                }
                #[cfg(not(windows))]
                target.focus();
            }
        });
    }
}
