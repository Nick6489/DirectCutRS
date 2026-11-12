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
