use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::{self, Receiver, TryRecvError},
};

use rungui::*;

use crate::{
    Clipboard,
    audio::{self, Playback},
    mp3::{self, Format, Frames, Result},
};

#[derive(Clone)]
struct Edit {
    frames: Frames,
    cursor: usize,
    start: Option<usize>,
    end: Option<usize>,
    revision: u64,
}

impl Edit {
    fn selection(&self) -> Result<std::ops::Range<usize>> {
        match (self.start, self.end) {
            (Some(start), Some(end)) if start < end => Ok(start..end),
            _ => Err("Set a start marker and an end marker after it.".into()),
        }
    }
}

struct Editor {
    window: Window,
    status: TextArea,
    activity: String,
    recovery: Option<String>,
    path: PathBuf,
    destination: Option<PathBuf>,
    format: Format,
    edit: Edit,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    next_revision: u64,
    saved_revision: u64,
    playback: Option<Playback>,
    clipboard: Clipboard,
    save: Option<Receiver<(PathBuf, u64, Result<()>)>>,
}

#[derive(Clone, Copy)]
enum Command {
    Open,
    Save,
    SaveAs,
    Close,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    Clear,
    Beginning,
    End,
    Move(i32, bool),
    StartMarker,
    EndMarker,
    GoStart,
    GoEnd,
    PlayStop,
    PausePlay,
    Status,
    Help,
}

pub fn choose_files(parent: Window, clipboard: &Clipboard) {
    let paths = FileDialog::new()
        .title("Open MP3 files")
        .filter("MP3 audio", &["mp3"])
        .open_many(Some(parent));
    for path in paths {
        open_file(parent, path, clipboard);
    }
}

pub fn open_file(parent: Window, path: PathBuf, clipboard: &Clipboard) {
    let (sender, receiver) = mpsc::channel();
    let input = path.clone();
    let started = std::thread::Builder::new()
        .name("MP3 import".into())
        .spawn(move || {
            let result = fs::read(input)
                .map_err(Into::into)
                .and_then(|bytes| mp3::parse(&bytes));
            let _ = sender.send(result);
        });
    if let Err(error) = started {
        show_error(
            parent,
            &format!("Could not open {}. {error}", path.display()),
        );
        return;
    }
    let clipboard = clipboard.clone();
    let timer = Rc::new(std::cell::Cell::new(None::<Timer>));
    let handle = Timer::every(50, {
        let timer = timer.clone();
        move || {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => Err("The import worker stopped.".into()),
            };
            if let Some(timer) = timer.take() {
                timer.stop();
            }
            match result {
                Ok((format, frames, recovery)) => {
                    create(path.clone(), format, frames, recovery, clipboard.clone())
                }
                Err(error) => show_error(
                    parent,
                    &format!("Could not open {}. {error}", path.display()),
                ),
            }
        }
    });
    timer.set(Some(handle));
}

fn name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn show_error(window: Window, error: &str) {
    message_box(
        Some(window),
        MessageKind::Error,
        Buttons::Ok,
        "DirectCut RS",
        error,
    );
}

fn create(
    path: PathBuf,
    format: Format,
    frames: Frames,
    recovery: mp3::Recovery,
    clipboard: Clipboard,
) {
    let window = Window::new("DirectCut RS");
    let column = VBox::new(window);
    column.set_padding(12);
    column.set_spacing(8);
    Label::new(column, "Timeline status (F6)");
    let status = TextArea::new(column);
    status.set_a11y_name("Timeline status");
    status.set_read_only(true);
    status.set_min_size(480, 125);
    let bar = MenuBar::new(window);
    let editor = Rc::new(RefCell::new(Editor {
        window,
        status,
        activity: "Opened MP3.".into(),
        recovery: (recovery != mp3::Recovery::default()).then(|| {
            format!(
                "Import recovery skipped {} stray or partial bytes and {} incomplete audio frames.",
                recovery.skipped_bytes, recovery.skipped_frames
            )
        }),
        path,
        destination: None,
        format,
        edit: Edit {
            frames,
            cursor: 0,
            start: None,
            end: None,
            revision: 0,
        },
        undo: Vec::new(),
        redo: Vec::new(),
        next_revision: 1,
        saved_revision: 0,
        playback: None,
        clipboard,
        save: None,
    }));
    let timer = Timer::every(75, {
        let editor = Rc::downgrade(&editor);
        move || {
            if let Some(editor) = editor.upgrade()
                && let Ok(mut editor) = editor.try_borrow_mut()
            {
                editor.poll();
            }
        }
    });
    use Command::*;
    for (menu, commands) in [
        (
            "&File",
            &[
                ("&Open MP3...", "Ctrl+O", Open),
                ("&Save Edited MP3", "Ctrl+S", Save),
                ("Save Edited MP3 &As...", "Ctrl+Shift+S", SaveAs),
                ("&Close", "Ctrl+W", Close),
            ][..],
        ),
        (
            "&Edit",
            &[
                ("&Undo", "Ctrl+Z", Undo),
                ("&Redo", "Ctrl+Y", Redo),
                ("Cu&t", "Ctrl+X", Cut),
                ("&Copy", "Ctrl+C", Copy),
                ("&Paste", "Ctrl+V", Paste),
                ("&Delete Selection", "Del", Delete),
                ("Select &All", "Ctrl+A", SelectAll),
                ("C&lear Markers", "Esc", Clear),
            ],
        ),
        (
            "&Navigate",
            &[
                ("File &Start", "Shift+Home", Beginning),
                ("File &End", "Shift+End", End),
                ("&Previous Frame", "Left", Move(-1, false)),
                ("&Next Frame", "Right", Move(1, false)),
                ("&Back One Second", "Ctrl+Left", Move(-1, true)),
                ("&Forward One Second", "Ctrl+Right", Move(1, true)),
                ("Set Start &Marker", "[", StartMarker),
                ("Set E&nd Marker", "]", EndMarker),
                ("&Go to Start Marker", "Ctrl+[", GoStart),
                ("Go to End Mar&ker", "Ctrl+]", GoEnd),
            ],
        ),
        (
            "&Transport",
            &[
                ("Play / &Stop", "Space", PlayStop),
                ("&Pause / Resume", "Ctrl+Space", PausePlay),
                ("Read Timeline St&atus", "F6", Status),
            ],
        ),
        ("&Help", &[("&Keyboard Shortcuts", "F1", Help)]),
    ] {
        let menu = Menu::new(bar, menu);
        for &(label, shortcut, command) in commands {
            let item = MenuItem::new(menu, label);
            item.set_accel(shortcut);
            let editor = editor.clone();
            item.on_click(move || match command {
                Close => {
                    if can_close(&editor) {
                        timer.stop();
                        window.close();
                    }
                }
                Open => {
                    let clipboard = editor.borrow().clipboard.clone();
                    choose_files(window, &clipboard);
                }
                _ => {
                    let result = editor.borrow_mut().run(command);
                    if let Err(error) = result {
                        show_error(window, &error.to_string());
                    }
                }
            });
        }
    }
    window.on_close({
        let editor = editor.clone();
        move || {
            let close = can_close(&editor);
            if close {
                timer.stop();
            }
            close
        }
    });
    editor.borrow().refresh();
    window.set_size(570, 230);
    window.show();
    status.focus();
}

fn can_close(editor: &RefCell<Editor>) -> bool {
    let (window, saving, dirty) = {
        let editor = editor.borrow();
        (editor.window, editor.save.is_some(), editor.dirty())
    };
    if saving {
        show_error(
            window,
            "An MP3 is being saved. Close this window after the save finishes.",
        );
        return false;
    }
    !dirty
        || message_box(
            Some(window),
            MessageKind::Warning,
            Buttons::OkCancel,
            "Discard unsaved edits?",
            "Close this window and discard unsaved edits?",
        ) == Answer::Ok
}

impl Editor {
    fn dirty(&self) -> bool {
        self.edit.revision != self.saved_revision
    }

    fn position(&self) -> usize {
        self.playback
            .as_ref()
            .map_or(self.edit.cursor, |p| p.position(self.format))
            .min(self.edit.frames.len())
    }

    fn status_text(&self) -> String {
        let time = |frame| {
            let millis = audio::duration(self.format, frame).as_millis();
            format!(
                "{}:{:02}.{:03}",
                millis / 60_000,
                millis / 1000 % 60,
                millis % 1000
            )
        };
        let marker = |frame: Option<usize>| frame.map_or("unset".into(), time);
        let transport = match &self.playback {
            Some(p) if p.sink.is_paused() => "Paused",
            Some(_) => "Playing",
            None => "Stopped",
        };
        format!(
            "{transport}. Position {} of {}.\r\nStart marker {}. End marker {}.\r\n{} Hz, {} {}. {}\r\n{}{}{}",
            time(self.position()),
            time(self.edit.frames.len()),
            marker(self.edit.start),
            marker(self.edit.end),
            self.format.rate,
            self.format.channels,
            if self.format.channels == 1 {
                "channel"
            } else {
                "channels"
            },
            if self.dirty() { "Unsaved edits." } else { "" },
            self.activity,
            if self.recovery.is_some() { "\r\n" } else { "" },
            self.recovery.as_deref().unwrap_or(""),
        )
    }

    fn refresh(&self) {
        let status = self.status_text();
        if self.status.text() != status {
            self.status.set_text(&status);
        }
        let title = format!(
            "{}{} - DirectCut RS",
            name(self.destination.as_deref().unwrap_or(&self.path)),
            if self.dirty() { " *" } else { "" },
        );
        if title != self.window.title() {
            self.window.set_title(&title);
        }
    }

    fn poll(&mut self) {
        if let Some(p) = &self.playback
            && p.sink.empty()
        {
            let position = self.position();
            let error = p.error.lock().unwrap().take();
            self.playback = None;
            self.edit.cursor = if error.is_some() {
                position
            } else {
                self.edit.frames.len()
            };
            if let Some(error) = error {
                self.activity = format!("Playback stopped. {error}");
            }
            self.refresh();
        }
        let Some(save) = &self.save else { return };
        match save.try_recv() {
            Ok((path, revision, result)) => {
                self.save = None;
                match result {
                    Ok(()) => {
                        self.activity = format!("Saved {}.", name(&path));
                        self.destination = Some(path);
                        self.saved_revision = revision;
                    }
                    Err(error) => {
                        self.activity = format!("Save failed. {error}");
                        let window = self.window;
                        let message = self.activity.clone();
                        App::post(move || {
                            if window.is_alive() {
                                show_error(window, &message);
                            }
                        });
                    }
                }
                self.refresh();
            }
            Err(TryRecvError::Disconnected) => {
                self.save = None;
                self.activity = "Save failed. The export worker stopped.".into();
                self.refresh();
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn stop_here(&mut self) {
        self.edit.cursor = self.position();
        self.playback = None;
    }

    fn seek(&mut self, position: usize) -> Result<()> {
        let playing = self.playback.as_ref().is_some_and(|p| !p.sink.is_paused());
        self.playback = None;
        self.edit.cursor = position.min(self.edit.frames.len());
        if playing && self.edit.cursor < self.edit.frames.len() {
            self.play()?;
        }
        Ok(())
    }

    fn play(&mut self) -> Result<()> {
        if self.edit.frames.is_empty() {
            return Err("The timeline is empty.".into());
        }
        if self.edit.cursor == self.edit.frames.len() {
            self.edit.cursor = 0;
        }
        self.playback = Some(Playback::start(
            self.format,
            &self.edit.frames,
            self.edit.cursor,
        )?);
        Ok(())
    }

    fn replace(&mut self, range: std::ops::Range<usize>, insert: Frames) {
        self.stop_here();
        if self.undo.len() == 32 {
            self.undo.remove(0);
        }
        self.undo.push(self.edit.clone());
        self.redo.clear();
        let start = range.start;
        let end = start + insert.len();
        self.edit.frames.splice(range, insert);
        self.edit.cursor = start;
        self.edit.start = (start != end).then_some(start);
        self.edit.end = (start != end).then_some(end);
        self.edit.revision = self.next_revision;
        self.next_revision += 1;
    }

    fn save(&mut self, choose_path: bool) -> Result<()> {
        if self.save.is_some() {
            return Err("An MP3 is already being saved from this window.".into());
        }
        if self.edit.frames.is_empty() {
            return Err("The timeline is empty.".into());
        }
        let path = if choose_path || self.destination.is_none() {
            let suggested = format!(
                "{} - edited.mp3",
                self.path
                    .file_stem()
                    .unwrap_or(self.path.as_os_str())
                    .to_string_lossy()
            );
            let Some(path) = FileDialog::new()
                .title("Save edited MP3")
                .filter("MP3 audio", &["mp3"])
                .file_name(&suggested)
                .save(Some(self.window))
            else {
                return Ok(());
            };
            path
        } else {
            self.destination.clone().unwrap()
        };
        let format = self.format;
        let frames = self.edit.frames.clone();
        let revision = self.edit.revision;
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("MP3 export".into())
            .spawn(move || {
                let result = audio::export(&path, format, frames);
                let _ = sender.send((path, revision, result));
            })?;
        self.save = Some(receiver);
        self.activity = "Validating and saving MP3...".into();
        Ok(())
    }

    fn run(&mut self, command: Command) -> Result<()> {
        use Command::*;
        match command {
            Open | Close => unreachable!(),
            Save | SaveAs => self.save(matches!(command, SaveAs))?,
            Beginning => self.seek(0)?,
            End => self.seek(self.edit.frames.len())?,
            Move(delta, seconds) => {
                let scale = if seconds {
                    (self.format.rate as usize).div_ceil(self.format.samples())
                } else {
                    1
                };
                self.seek(
                    self.position()
                        .saturating_add_signed(delta as isize * scale as isize),
                )?;
            }
            StartMarker => self.edit.start = Some(self.position()),
            EndMarker => self.edit.end = Some(self.position()),
            GoStart | GoEnd => {
                let marker = if matches!(command, GoStart) {
                    self.edit.start
                } else {
                    self.edit.end
                };
                self.seek(marker.ok_or("That marker is unset.")?)?;
            }
            Clear => {
                self.edit.start = None;
                self.edit.end = None;
            }
            SelectAll => {
                self.edit.start = Some(0);
                self.edit.end = Some(self.edit.frames.len());
            }
            Cut | Copy | Delete => {
                let range = self.edit.selection()?;
                if !matches!(command, Delete) {
                    *self.clipboard.borrow_mut() =
                        Some((self.format, self.edit.frames[range.clone()].to_vec()));
                    self.activity = "Copied selection to the DirectCut clipboard.".into();
                }
                if !matches!(command, Copy) {
                    self.replace(range, Vec::new());
                    self.activity = if matches!(command, Cut) {
                        "Cut selection."
                    } else {
                        "Deleted selection."
                    }
                    .into();
                }
            }
            Paste => {
                let (format, frames) = self
                    .clipboard
                    .borrow()
                    .clone()
                    .ok_or("The DirectCut clipboard is empty.")?;
                if format != self.format {
                    return Err(
                        "Paste requires the same MPEG version, sample rate, and channel count."
                            .into(),
                    );
                }
                let position = self.position();
                let range = self.edit.selection().unwrap_or(position..position);
                self.replace(range, frames);
                self.activity = "Pasted audio.".into();
            }
            Undo => {
                if let Some(edit) = self.undo.pop() {
                    self.stop_here();
                    self.redo.push(std::mem::replace(&mut self.edit, edit));
                }
            }
            Redo => {
                if let Some(edit) = self.redo.pop() {
                    self.stop_here();
                    self.undo.push(std::mem::replace(&mut self.edit, edit));
                }
            }
            PlayStop => {
                if let Some(playback) = self.playback.take() {
                    self.edit.cursor = playback.start;
                } else {
                    self.play()?;
                }
            }
            PausePlay => match &self.playback {
                Some(p) if p.sink.is_paused() => p.sink.play(),
                Some(p) => p.sink.pause(),
                None => self.play()?,
            },
            Status => {
                message_box(
                    Some(self.window),
                    MessageKind::Info,
                    Buttons::Ok,
                    "Timeline status",
                    &self.status_text(),
                );
            }
            Help => {
                message_box(
                    Some(self.window),
                    MessageKind::Info,
                    Buttons::Ok,
                    "Keyboard shortcuts",
                    "Space plays or stops. Ctrl+Space pauses or resumes.\r\n\
                     Left and Right move a frame. Ctrl+Left and Ctrl+Right move about a second.\r\n\
                     Shift+Home and Shift+End move to the file boundaries.\r\n\
                     [ sets the start marker. ] sets the end marker. Escape clears both.\r\n\
                     Ctrl+X cuts, Ctrl+C copies, Ctrl+V pastes, and Delete removes the selection.\r\n\
                     Ctrl+Z undoes and Ctrl+Y redoes edits. Ctrl+S saves.\r\n\
                     F6 reads the timeline status. All commands are also in the menus.",
                );
            }
        }
        self.refresh();
        Ok(())
    }
}
