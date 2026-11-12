use crate::{
    Session,
    editor::{self, show_error},
    stream::{self, TrimResult},
};
use rungui::*;
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::{self, Receiver, TryRecvError},
};

fn show_and_focus(window: Window, target: Widget) {
    window.show();
    for delay in [0, 100] {
        Timer::once(delay, move || {
            if !window.is_alive() {
                return;
            }
            if target.is_alive() {
                target.focus();
            } else {
                window.focus();
            }
        });
    }
}

fn park_focus_before_disable(disabled: Widget, fallback: Widget) {
    if hwnd_of(disabled).is_some_and(|hwnd| focus_hwnd() == Some(hwnd)) {
        fallback.focus();
    }
    disabled.set_enabled(false);
}

/// The HWND of a widget, for comparing against the keyboard focus owner.
fn hwnd_of(widget: Widget) -> Option<isize> {
    match widget.native_handle() {
        Some(NativeHandle::Win32(hwnd)) => Some(hwnd as isize),
        _ => None,
    }
}

#[cfg(windows)]
fn focus_hwnd() -> Option<isize> {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetFocus() -> isize;
    }
    let focus = unsafe { GetFocus() };
    (focus != 0).then_some(focus)
}

#[cfg(not(windows))]
fn focus_hwnd() -> Option<isize> {
    None
}

pub fn trim_window(_parent: Window, initial: Option<PathBuf>, session: Session) {
    let window = Window::new("Trim Stream Archive");
    let column = VBox::new(window);
    column.set_padding(12);
    column.set_spacing(8);

    Label::new(
        column,
        "Choose an archived MP3 with a matching CUE sheet. The source is never changed.",
    );

    let file_row = HBox::new(column);
    file_row.set_spacing(8);
    Label::new(file_row, "Archive MP3:");
    let file = TextInput::new(file_row);
    file.set_a11y_name("Archive MP3 file");
    file.set_expand(1.0);
    file.set_min_size(360, 0);
    let browse = Button::new(file_row, "&Browse...");

    let start_row = HBox::new(column);
    start_row.set_spacing(8);
    Label::new(start_row, "First title to keep:");
    let first = ComboBox::new(start_row);
    first.set_a11y_name("First title to keep");
    first.set_expand(1.0);
    let end_row = HBox::new(column);
    end_row.set_spacing(8);
    Label::new(end_row, "Last title to keep:");
    let last = ComboBox::new(end_row);
    last.set_a11y_name("Last title to keep");
    last.set_expand(1.0);

    let info = Label::new(column, "Choose an archive with a matching .cue file.");
    let buttons = HBox::new(column);
    buttons.set_spacing(8);
    let trim = Button::new(buttons, "&Trim Losslessly");
    trim.set_enabled(false);
    let open = Button::new(buttons, "Open Trimmed &Audio");
    open.set_enabled(false);
    let close = Button::new(buttons, "&Close");

    let state = Rc::new(RefCell::new(TrimUi {
        window,
        file,
        first,
        last,
        info,
        trim,
        open,
        session,
        entries: Vec::new(),
        result: None,
        receiver: None,
        timer: None,
    }));
    let timer = {
        let state = state.clone();
        Timer::every(150, move || {
            if let Ok(mut state) = state.try_borrow_mut() {
                state.poll();
            }
        })
    };
    state.borrow_mut().timer = Some(timer);

    if let Some(path) = initial {
        state.borrow_mut().load(&path);
    }

    {
        let state = state.clone();
        browse.on_click(move || {
            let initial = {
                let state = state.borrow();
                let text = state.file.text();
                (!text.trim().is_empty()).then_some(text)
            };
            let mut dialog = FileDialog::new()
                .title("Open archived MP3")
                .filter("MP3 audio", &["mp3"]);
            if let Some(initial) = initial {
                let path = PathBuf::from(initial);
                if let Some(parent) = path.parent() {
                    dialog = dialog.directory(&parent.to_string_lossy());
                }
            }
            if let Some(path) = dialog.open(Some(window))
                && let Ok(mut state) = state.try_borrow_mut()
            {
                state.load(&path);
            }
        });
    }
    {
        let state = state.clone();
        trim.on_click(move || {
            if let Ok(mut state) = state.try_borrow_mut() {
                state.start();
            }
        });
    }
    {
        let state = state.clone();
        open.on_click(move || {
            if let Ok(state) = state.try_borrow()
                && let Some(result) = &state.result
            {
                editor::open_file(state.window, result.audio.clone(), &state.session);
            }
        });
    }
    close.on_click(move || window.close());
    window.on_close({
        let state = state.clone();
        move || {
            if let Ok(state) = state.try_borrow()
                && let Some(timer) = &state.timer
            {
                timer.stop();
            }
            true
        }
    });
    window.set_size(760, 330);
    show_and_focus(window, *state.borrow().file);
}

struct TrimUi {
    window: Window,
    file: TextInput,
    first: ComboBox,
    last: ComboBox,
    info: Label,
    trim: Button,
    open: Button,
    session: Session,
    entries: Vec<(f64, String)>,
    result: Option<TrimResult>,
    receiver: Option<Receiver<std::result::Result<TrimResult, String>>>,
    timer: Option<Timer>,
}

impl TrimUi {
    fn load(&mut self, path: &Path) {
        self.file.set_text(&path.to_string_lossy());
        self.result = None;
        park_focus_before_disable(*self.open, *self.file);

        park_focus_before_disable(*self.trim, *self.file);

        self.entries.clear();
        self.first.set_items::<&str>(&[]);
        self.last.set_items::<&str>(&[]);
        let cue = path.with_extension("cue");
        if !cue.exists() {
            self.info.set_text(&format!(
                "No matching CUE sheet was found next to {}.",
                path.file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
            ));
            return;
        }
        match stream::read_cue(&cue) {
            Ok(entries) if !entries.is_empty() => {
                self.entries = entries;
                let items: Vec<String> = self
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(index, (seconds, title))| {
                        format!(
                            "{:02}  {}  {}",
                            index + 1,
                            stream::cue_time(*seconds),
                            title
                        )
                    })
                    .collect();
                self.first.set_items(&items);
                self.last.set_items(&items);
                self.first.set_selected(Some(0));
                self.last.set_selected(Some(items.len() - 1));
                self.trim.set_enabled(true);
                self.info.set_text(&format!(
                    "{} title(s) found. The trim ends at the next title after the one selected as last.",
                    items.len()
                ));
            }
            Ok(_) => self
                .info
                .set_text("The CUE sheet contains no track entries."),
            Err(error) => self
                .info
                .set_text(&format!("Could not read the CUE sheet. {error}")),
        }
    }

    fn start(&mut self) {
        if self.receiver.is_some() {
            return;
        }
        let path = PathBuf::from(self.file.text());
        if !path.is_file() {
            show_error(self.window, "Choose an existing archived MP3 file.");
            return;
        }
        let cue = path.with_extension("cue");
        if !cue.is_file() {
            show_error(
                self.window,
                "A matching .cue file is required next to the MP3.",
            );
            return;
        }
        let (Some(first), Some(last)) = (self.first.selected(), self.last.selected()) else {
            show_error(self.window, "Choose the first and last title to keep.");
            return;
        };
        if last < first {
            show_error(
                self.window,
                "The last title must not come before the first title.",
            );
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("MP3 archive trim".into())
            .spawn(move || {
                let _ = sender.send(stream::trim_mp3(&path, &cue, first, last));
            });
        if let Err(error) = worker {
            show_error(
                self.window,
                &format!("Could not start the trim worker. {error}"),
            );
            return;
        }
        self.receiver = Some(receiver);
        park_focus_before_disable(*self.trim, *self.file);

        self.info.set_text("Trimming and validating the new MP3...");
    }

    fn poll(&mut self) {
        let Some(receiver) = &self.receiver else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.receiver = None;
                self.trim.set_enabled(true);
                show_error(self.window, "The trim worker stopped without a result.");
                return;
            }
        };
        self.receiver = None;
        self.trim.set_enabled(true);
        match result {
            Ok(result) => {
                let mut message = format!(
                    "Trimmed {} frame(s), {:.3} to {:.3} seconds. Saved {}.",
                    result.frames,
                    result.start,
                    result.end,
                    result
                        .audio
                        .file_name()
                        .unwrap_or(result.audio.as_os_str())
                        .to_string_lossy()
                );
                if let Some(cue) = &result.cue {
                    message.push_str(&format!(
                        " CUE: {}.",
                        cue.file_name().unwrap_or(cue.as_os_str()).to_string_lossy()
                    ));
                }
                if let Some(csv) = &result.csv {
                    message.push_str(&format!(
                        " Titles: {}.",
                        csv.file_name().unwrap_or(csv.as_os_str()).to_string_lossy()
                    ));
                }
                if let Some(warning) = &result.csv_warning {
                    message.push_str(" Warning: ");
                    message.push_str(warning);
                }
                self.info.set_text(&message);
                self.open.set_enabled(true);
                self.result = Some(result);
            }
            Err(error) => {
                self.info.set_text(&error);
                show_error(self.window, &error);
            }
        }
    }
}
