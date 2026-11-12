use rungui::Window;

pub fn confirm_paste(owner: Window, message: &str) -> (bool, bool) {
    ask(owner, message, 101)
}

pub fn confirm_split(owner: Window, message: &str) -> bool {
    ask(owner, message, 102).0
}

#[cfg(windows)]
fn ask(owner: Window, message: &str, resource: usize) -> (bool, bool) {
    use rungui::NativeHandle;
    use windows::{
        Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, WPARAM},
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                BM_GETCHECK, DialogBoxParamW, EndDialog, GetDlgItem, IDCANCEL, IDOK, SendMessageW,
                SetDlgItemTextW, WM_CLOSE, WM_COMMAND, WM_INITDIALOG,
            },
        },
        core::PCWSTR,
    };

    struct State {
        message: Vec<u16>,
        remember: bool,
        paste: bool,
    }

    unsafe extern "system" fn dialog(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> isize {
        use windows::Win32::UI::WindowsAndMessaging::{
            GWLP_USERDATA, GetWindowLongPtrW, SetWindowLongPtrW,
        };
        unsafe {
            match message {
                WM_INITDIALOG => {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
                    let state = &*(lparam.0 as *const State);
                    let _ = SetDlgItemTextW(hwnd, 1001, PCWSTR(state.message.as_ptr()));
                    1
                }
                WM_COMMAND if wparam.0 as u16 as i32 == IDOK.0 => {
                    let state = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State);
                    if state.paste {
                        state.remember =
                            SendMessageW(GetDlgItem(hwnd, 1002), BM_GETCHECK, WPARAM(0), LPARAM(0))
                                .0
                                == 1;
                    }
                    let _ = EndDialog(hwnd, IDOK.0 as isize);
                    1
                }
                WM_CLOSE => {
                    let _ = EndDialog(hwnd, IDCANCEL.0 as isize);
                    1
                }
                WM_COMMAND if wparam.0 as u16 as i32 == IDCANCEL.0 => {
                    let _ = EndDialog(hwnd, IDCANCEL.0 as isize);
                    1
                }
                _ => 0,
            }
        }
    }

    let Some(NativeHandle::Win32(hwnd)) = owner.native_handle() else {
        return (false, false);
    };
    let mut state = State {
        message: message.encode_utf16().chain([0]).collect(),
        remember: false,
        paste: resource == 101,
    };
    let result = unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return (false, false);
        };
        DialogBoxParamW(
            HINSTANCE(module.0),
            PCWSTR(resource as *const u16),
            HWND(hwnd as isize),
            Some(dialog),
            LPARAM(&mut state as *mut State as isize),
        )
    };
    (result == IDOK.0 as isize, state.remember)
}

#[cfg(not(windows))]
fn ask(owner: Window, message: &str, _: usize) -> (bool, bool) {
    use rungui::{Answer, Buttons, MessageKind, message_box};
    (
        message_box(
            Some(owner),
            MessageKind::Warning,
            Buttons::OkCancel,
            "Confirm",
            message,
        ) == Answer::Ok,
        false,
    )
}

#[cfg(windows)]
pub use about::{AboutDialog, show_about};

#[cfg(windows)]
mod about {
    use rungui::{NativeHandle, Window};
    use windows::{
        Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, WPARAM},
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                DialogBoxParamW, EndDialog, GWLP_USERDATA, GetWindowLongPtrW, IDCANCEL,
                MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW, SetDlgItemTextW,
                SetWindowLongPtrW, WM_CLOSE, WM_COMMAND, WM_INITDIALOG,
            },
        },
        core::{PCWSTR, w},
    };

    const ABOUT_DIALOG: usize = 103;
    const ABOUT_TEXT: i32 = 1001;
    const CHECK_FOR_UPDATES: i32 = 1002;
    const UPDATE_STATUS: i32 = 1003;

    #[derive(Clone, Copy)]
    pub struct AboutDialog(HWND);

    impl AboutDialog {
        pub fn set_status(self, text: &str) {
            let text = wide(text);
            unsafe {
                let _ = SetDlgItemTextW(self.0, UPDATE_STATUS, PCWSTR(text.as_ptr()));
            }
        }

        pub fn close(self) {
            unsafe {
                let _ = EndDialog(self.0, IDCANCEL.0 as isize);
            }
        }

        pub fn show_update_result(self, text: &str, error: bool) {
            let text = wide(text);
            let icon = if error {
                MB_ICONERROR
            } else {
                MB_ICONINFORMATION
            };
            unsafe {
                MessageBoxW(
                    self.0,
                    PCWSTR(text.as_ptr()),
                    w!("DirectCut RS updates"),
                    MB_OK | icon,
                );
            }
        }
    }

    struct State<'a> {
        text: Vec<u16>,
        initialized: &'a dyn Fn(AboutDialog),
        check: &'a dyn Fn(),
    }

    pub fn show_about(
        owner: Window,
        text: &str,
        initialized: impl Fn(AboutDialog),
        check: impl Fn(),
    ) -> bool {
        let Some(NativeHandle::Win32(owner)) = owner.native_handle() else {
            return false;
        };
        let state = State {
            text: wide(text),
            initialized: &initialized,
            check: &check,
        };
        unsafe {
            let Ok(module) = GetModuleHandleW(None) else {
                return false;
            };
            DialogBoxParamW(
                HINSTANCE(module.0),
                PCWSTR(ABOUT_DIALOG as *const u16),
                HWND(owner as isize),
                Some(dialog_proc),
                LPARAM(&state as *const State<'_> as isize),
            ) != -1
        }
    }

    unsafe extern "system" fn dialog_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> isize {
        unsafe {
            match message {
                WM_INITDIALOG => {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
                    let state = &*(lparam.0 as *const State<'_>);
                    let _ = SetDlgItemTextW(hwnd, ABOUT_TEXT, PCWSTR(state.text.as_ptr()));
                    (state.initialized)(AboutDialog(hwnd));
                    1
                }
                WM_COMMAND if wparam.0 as u16 as i32 == CHECK_FOR_UPDATES => {
                    let state = &*(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State<'_>);
                    AboutDialog(hwnd).set_status("Checking for updates...");
                    (state.check)();
                    1
                }
                WM_COMMAND if wparam.0 as u16 as i32 == IDCANCEL.0 => {
                    AboutDialog(hwnd).close();
                    1
                }
                WM_CLOSE => {
                    AboutDialog(hwnd).close();
                    1
                }
                _ => 0,
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }
}
