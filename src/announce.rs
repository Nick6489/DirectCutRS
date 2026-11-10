//! Screen-reader announcements through Windows UI Automation notifications.
//!
//! This is deliberately not a speech synthesizer. If no accessibility client is
//! listening, the call does nothing. There is no SAPI, OneCore, or other TTS
//! fallback, so a machine without a screen reader stays silent.
//!
//! `UiaRaiseNotificationEvent` arrived in Windows 10 version 1709 and is the
//! only UIA entry point resolved at runtime here, so older systems still start
//! and simply run without announcements.
//!
//! Notifications must be raised on a real provider node. Microsoft's docs for
//! `UiaHostProviderFromHwnd` state that its host provider "cannot be used to
//! raise events", so [`attach`] exposes a minimal pass-through
//! `IRawElementProviderSimple` for the window through `WM_GETOBJECT`, and
//! announcements are raised on that provider.

use rungui::Window;

/// Expose the announcement provider on `window`. Call once when the window is
/// created, before an accessibility client queries it.
pub fn attach(window: Window) {
    #[cfg(windows)]
    unsafe {
        windows::attach(window)
    };
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}

pub fn announce(window: Window, text: &str) {
    if text.is_empty() {
        return;
    }
    #[cfg(windows)]
    unsafe {
        windows::announce(window, text)
    };
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}

#[cfg(test)]
mod tests {
    use super::announce;
    use rungui::{WidgetId, Window};

    #[test]
    fn announcing_to_a_dead_window_does_nothing() {
        announce(Window::from_id(WidgetId::DEAD), "This must stay silent.");
    }
}

#[cfg(windows)]
mod windows {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::{Mutex, OnceLock};

    use rungui::{NativeHandle, Window};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Accessibility::{
        IRawElementProviderSimple, IRawElementProviderSimple_Impl, ProviderOptions,
        ProviderOptions_ServerSideProvider, UIA_PATTERN_ID, UIA_PROPERTY_ID,
        UiaClientsAreListening, UiaHostProviderFromHwnd, UiaReturnRawElementProvider,
        UiaRootObjectId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, GWLP_WNDPROC, GetWindowLongPtrW, SetWindowLongPtrW,
        WM_DESTROY, WM_GETOBJECT,
    };
    use windows::core::{BSTR, IUnknown, Interface, Result as ComResult, VARIANT, implement, w};

    const NOTIFICATION_ACTION_COMPLETED: i32 = 2;
    const PROCESSING_IMPORTANT_MOST_RECENT: i32 = 1;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }

    type RaiseNotification =
        unsafe extern "system" fn(*mut c_void, i32, i32, *const u16, *const u16) -> i32;

    /// `UiaRaiseNotificationEvent` is the only announcement entry point that
    /// requires Windows 10 version 1709, so it alone is resolved at runtime.
    static RAISE: OnceLock<Option<RaiseNotification>> = OnceLock::new();

    fn raise_notification() -> Option<RaiseNotification> {
        *RAISE.get_or_init(|| unsafe {
            let name = w!("uiautomationcore.dll");
            let module = LoadLibraryW(name.as_ptr());
            if module.is_null() {
                return None;
            }
            let address = GetProcAddress(module, c"UiaRaiseNotificationEvent".as_ptr().cast());
            if address.is_null() {
                return None;
            }
            Some(std::mem::transmute_copy(&address))
        })
    }

    /// A minimal pass-through provider. It overrides no properties and no
    /// patterns, so the tree that screen readers see is still the host window
    /// element's tree; it exists so notifications have a real provider node to
    /// be raised on.
    #[implement(IRawElementProviderSimple)]
    struct Provider {
        hwnd: isize,
    }

    impl IRawElementProviderSimple_Impl for Provider {
        fn ProviderOptions(&self) -> ComResult<ProviderOptions> {
            Ok(ProviderOptions_ServerSideProvider)
        }

        fn GetPatternProvider(&self, _patternid: UIA_PATTERN_ID) -> ComResult<IUnknown> {
            // UIA queries the returned object for the requested pattern; the
            // host window element implements none, so this is the COM-safe way
            // to report "no pattern".
            self.HostRawElementProvider()?.cast::<IUnknown>()
        }

        fn GetPropertyValue(&self, _propertyid: UIA_PROPERTY_ID) -> ComResult<VARIANT> {
            // VT_EMPTY defers every property to the host window element.
            Ok(VARIANT::default())
        }

        fn HostRawElementProvider(&self) -> ComResult<IRawElementProviderSimple> {
            unsafe { UiaHostProviderFromHwnd(HWND(self.hwnd)) }
        }
    }

    struct Attached {
        provider: IRawElementProviderSimple,
        previous: WindowProc,
    }

    // The provider interface reference is only ever touched on the window's
    // own thread (attach, WM_GETOBJECT, and announce all run there); the Mutex
    // serializes that access.
    unsafe impl Send for Attached {}

    type WindowProc = Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>;

    static ATTACHED: OnceLock<Mutex<HashMap<isize, Attached>>> = OnceLock::new();

    fn attached() -> &'static Mutex<HashMap<isize, Attached>> {
        ATTACHED.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub unsafe fn attach(window: Window) {
        let Some(NativeHandle::Win32(hwnd)) = window.native_handle() else {
            return;
        };
        let key = hwnd as isize;
        let mut attached = attached().lock().unwrap();
        if attached.contains_key(&key) {
            return;
        }
        let h = HWND(hwnd as isize);
        let previous = unsafe { GetWindowLongPtrW(h, GWLP_WNDPROC) };
        attached.insert(
            key,
            Attached {
                provider: Provider { hwnd: key }.into(),
                previous: unsafe { std::mem::transmute::<isize, WindowProc>(previous) },
            },
        );
        drop(attached);
        unsafe {
            SetWindowLongPtrW(
                h,
                GWLP_WNDPROC,
                std::mem::transmute::<WindowProc, isize>(Some(wndproc)),
            );
        }
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let key = hwnd.0;
        if msg == WM_GETOBJECT
            && lparam.0 as i32 == UiaRootObjectId
            && let Some(provider) = attached()
                .lock()
                .unwrap()
                .get(&key)
                .map(|a| a.provider.clone())
        {
            unsafe {
                return UiaReturnRawElementProvider(hwnd, wparam, lparam, &provider);
            }
        }
        let previous = attached().lock().unwrap().get(&key).map(|a| a.previous);
        if msg == WM_DESTROY
            && let Some(entry) = attached().lock().unwrap().remove(&key)
        {
            // Tell UIA the window is gone, releasing its raised-event map.
            unsafe {
                let _ = UiaReturnRawElementProvider(hwnd, WPARAM(0), LPARAM(0), None);
            }
            let _ = entry;
        }
        match previous {
            Some(previous) => unsafe { CallWindowProcW(previous, hwnd, msg, wparam, lparam) },
            None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    pub unsafe fn announce(window: Window, text: &str) {
        if !unsafe { UiaClientsAreListening() }.as_bool() {
            return;
        }
        let Some(NativeHandle::Win32(hwnd)) = window.native_handle() else {
            return;
        };
        let key = hwnd as isize;
        let provider = attached()
            .lock()
            .unwrap()
            .get(&key)
            .map(|a| a.provider.clone());
        let Some(provider) = provider else {
            return;
        };
        let Some(raise) = raise_notification() else {
            return;
        };
        let display = BSTR::from(text);
        let activity = BSTR::from("DirectCut RS");
        unsafe {
            raise(
                provider.as_raw(),
                NOTIFICATION_ACTION_COMPLETED,
                PROCESSING_IMPORTANT_MOST_RECENT,
                display.as_ptr() as *mut u16,
                activity.as_ptr() as *mut u16,
            );
        }
    }
}
