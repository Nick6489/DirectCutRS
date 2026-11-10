use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::{self, Receiver, TryRecvError},
};

use rungui::*;

use crate::{
    OpenDocument, Session, announce,
    audio::{self, Playback},
    mp3::{self, Format, Frames, Result},
    report, stream, stream_window,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct TitleMarker {
    frame: usize,
    title: String,
}

#[derive(Clone)]
struct Edit {
    frames: Frames,
    titles: Vec<TitleMarker>,
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

const SPEED_MIN: f32 = 0.25;
const SPEED_MAX: f32 = 4.0;

/// A scrub move plays about this much audio from the new position, so moving
/// the playhead is audible even while playback is stopped.
const SCRUB_SNAP_SECONDS: f64 = 0.1;

fn time_text(format: Format, frame: usize) -> String {
    let millis = audio::duration(format, frame).as_millis();
    format!(
        "{}:{:02}.{:03}",
        millis / 60_000,
        millis / 1000 % 60,
        millis % 1000
    )
}

fn frame_seconds(format: Format, frame: usize) -> f64 {
    frame as f64 * format.samples() as f64 / f64::from(format.rate)
}

/// Frames per second, rounded up. This is the largest scrub step: one second.
fn frames_per_second(format: Format) -> usize {
    (format.rate as usize).div_ceil(format.samples())
}

/// The playback speeds offered in the Transport menu, from the minimum to the
/// maximum in 0.25x steps.
fn speed_steps() -> impl Iterator<Item = f32> {
    (0..16).map(|step| SPEED_MIN + 0.25 * step as f32)
}

/// The number of frames in a scrub snippet: about 0.1 seconds, at least one.
fn scrub_snap_frames(format: Format) -> usize {
    ((SCRUB_SNAP_SECONDS * f64::from(format.rate) / format.samples() as f64).ceil() as usize).max(1)
}

/// The spoken and displayed form of a scrub step, such as `1 frame (26 ms)`.
fn scrub_text(step: usize, format: Format) -> String {
    let millis =
        (step as f64 * format.samples() as f64 / f64::from(format.rate) * 1000.0).round() as u64;
    if step == 1 {
        format!("1 frame ({millis} ms)")
    } else {
        format!("{step} frames ({millis} ms)")
    }
}

/// The scrub step after zooming. Zooming in halves the step, stopping at one
/// frame; zooming out doubles it. The caller clamps to one second of frames.
fn zoomed_scrub_step(step: usize, zoom_in: bool) -> usize {
    if zoom_in { (step / 2).max(1) } else { step * 2 }
}

/// Convert CUE start times into frame-aligned title markers. Entries outside the
/// timeline are dropped. If two titles share a frame, the later title wins,
/// which is the same rule used when an edit deletes a whole title.
fn cue_titles(entries: Vec<(f64, String)>, format: Format, total: usize) -> Vec<TitleMarker> {
    let mut markers = Vec::with_capacity(entries.len());
    for (seconds, title) in entries {
        if total == 0 || !seconds.is_finite() || seconds < 0.0 {
            continue;
        }
        let frame = (seconds * f64::from(format.rate) / format.samples() as f64).round();
        if frame >= total as f64 {
            continue;
        }
        let title = title.trim();
        markers.push(TitleMarker {
            frame: frame as usize,
            title: if title.is_empty() {
                "Untitled".into()
            } else {
                title.into()
            },
        });
    }
    markers.sort_by_key(|marker| marker.frame);
    dedupe_titles(markers)
}

fn dedupe_titles(markers: Vec<TitleMarker>) -> Vec<TitleMarker> {
    let mut result: Vec<TitleMarker> = Vec::with_capacity(markers.len());
    for marker in markers {
        if result.last().is_some_and(|last| last.frame == marker.frame) {
            result.pop();
        }
        result.push(marker);
    }
    result
}

/// Move markers through a frame splice. A marker at the splice start survives;
/// titles strictly inside the replaced range disappear; later titles shift by
/// the length change. Duplicate starts keep the later title.
fn splice_titles(
    titles: &mut Vec<TitleMarker>,
    start: usize,
    end: usize,
    inserted: usize,
    total: usize,
) {
    let delta = inserted as isize - (end - start) as isize;
    let mut moved = Vec::with_capacity(titles.len());
    for marker in titles.drain(..) {
        let frame = if marker.frame <= start {
            marker.frame
        } else if marker.frame >= end {
            marker.frame.saturating_add_signed(delta)
        } else {
            continue;
        };
        if frame < total {
            moved.push(TitleMarker {
                frame,
                title: marker.title,
            });
        }
    }
    *titles = dedupe_titles(moved);
}

/// Titles that survive cropping to `range`, rebased so the crop starts at 0.
/// A crop that starts inside a title synthesizes that title at the new start.
fn titles_in_range(titles: &[TitleMarker], range: &std::ops::Range<usize>) -> Vec<TitleMarker> {
    let mut result = Vec::new();
    if let Some(holder) = titles
        .iter()
        .rev()
        .find(|marker| marker.frame <= range.start)
    {
        result.push(TitleMarker {
            frame: 0,
            title: holder.title.clone(),
        });
    }
    for marker in titles
        .iter()
        .filter(|marker| marker.frame > range.start && marker.frame < range.end)
    {
        result.push(TitleMarker {
            frame: marker.frame - range.start,
            title: marker.title.clone(),
        });
    }
    result
}

fn cue_entries(titles: &[TitleMarker], format: Format, total: usize) -> Vec<(f64, String)> {
    titles
        .iter()
        .filter(|marker| marker.frame < total)
        .map(|marker| (frame_seconds(format, marker.frame), marker.title.clone()))
        .collect()
}

/// The title containing `frame`: the last marker at or before it, if any.
fn title_at_frame(titles: &[TitleMarker], frame: usize) -> Option<usize> {
    titles.iter().rposition(|marker| marker.frame <= frame)
}

/// Index of the title before the one containing `frame`. Playback may already
/// have moved past a title start, so navigation is relative to the containing
/// title, not the raw playhead frame. At or before the first title this is
/// `None`, meaning go to the start of the timeline.
fn previous_title_index(titles: &[TitleMarker], frame: usize, total: usize) -> Option<usize> {
    if titles.is_empty() {
        return None;
    }
    if frame >= total {
        return Some(titles.len() - 1);
    }
    match title_at_frame(titles, frame) {
        Some(0) | None => None,
        Some(index) => Some(index - 1),
    }
}

/// Index of the title after the one containing `frame`. Before the first title
/// this is the first title; at or past the end of the timeline it is `None`,
/// meaning go to the end of the timeline.
fn next_title_index(titles: &[TitleMarker], frame: usize, total: usize) -> Option<usize> {
    if titles.is_empty() || frame >= total {
        return None;
    }
    match title_at_frame(titles, frame) {
        Some(index) => (index + 1 < titles.len()).then_some(index + 1),
        None => Some(0),
    }
}

/// How long after a title jump a marker command still snaps to that title's
/// start. Long enough to react to the jump announcement, short enough that a
/// deliberate mid-title marker is not hijacked.
const TITLE_MARKER_SNAP_SECONDS: f64 = 2.0;

/// The frame a marker command should use. Right after a title jump the playhead
/// has already moved past the title start, so use the navigation anchor while
/// the playhead is still inside that title and close to it.
fn marker_frame(
    titles: &[TitleMarker],
    format: Format,
    anchor: Option<usize>,
    position: usize,
) -> usize {
    let Some(anchor) = anchor else {
        return position;
    };
    let limit = (TITLE_MARKER_SNAP_SECONDS * f64::from(format.rate) / format.samples() as f64)
        .round() as usize;
    if position >= anchor
        && position - anchor <= limit
        && title_at_frame(titles, anchor) == title_at_frame(titles, position)
    {
        anchor
    } else {
        position
    }
}

#[derive(Debug, PartialEq)]
enum JumpTarget {
    Time(f64),
    Title(usize),
}

fn parse_jump(text: &str) -> Result<JumpTarget> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Enter a time, or t and a title number such as t24.".into());
    }
    if text.len() > 1 && matches!(text.as_bytes()[0], b't' | b'T') {
        let number: usize = text[1..]
            .trim()
            .parse()
            .map_err(|_| "Use t and a title number, for example t24.".to_string())?;
        if number == 0 {
            return Err("Title numbers start at 1.".into());
        }
        return Ok(JumpTarget::Title(number));
    }
    parse_timestamp(text).map(JumpTarget::Time)
}

/// Parse `ss`, `mm:ss`, or `hh:mm:ss` into seconds. Fractions are allowed in
/// the final part. A bare seconds value may exceed 59.
fn parse_timestamp(text: &str) -> Result<f64> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Enter a time as ss, mm:ss, or hh:mm:ss.".into());
    }
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() > 3 {
        return Err("Use ss, mm:ss, or hh:mm:ss.".into());
    }
    let mut values = Vec::with_capacity(parts.len());
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            return Err("Each time part needs a number.".into());
        }
        let value: f64 = part
            .parse()
            .map_err(|_| format!("\"{part}\" is not a number."))?;
        if !value.is_finite() || value < 0.0 {
            return Err("Time values must be zero or greater.".into());
        }
        values.push(value);
    }
    let seconds = match values.as_slice() {
        [seconds] => *seconds,
        [minutes, seconds] => {
            if *seconds >= 60.0 {
                return Err("Seconds must be less than 60 in mm:ss.".into());
            }
            minutes * 60.0 + seconds
        }
        [hours, minutes, seconds] => {
            if *minutes >= 60.0 || *seconds >= 60.0 {
                return Err("Minutes and seconds must be less than 60 in hh:mm:ss.".into());
            }
            hours * 3600.0 + minutes * 60.0 + seconds
        }
        _ => unreachable!(),
    };
    Ok(seconds)
}

struct Insertion {
    path: PathBuf,
    position: usize,
    result: Result<mp3::Scan>,
}

struct Opened {
    scan: mp3::Scan,
    cue: Option<PathBuf>,
    cue_entries: Vec<(f64, String)>,
    cue_error: Option<String>,
}

struct ExportOutcome {
    cue: Option<PathBuf>,
    cue_error: Option<String>,
}

type ExportResult = std::result::Result<ExportOutcome, String>;
type SaveMessage = (PathBuf, u64, ExportResult);
type SelectionMessage = (PathBuf, ExportResult);

struct Editor {
    window: Window,
    status: TextArea,
    activity: String,
    recovery: Option<String>,
    path: PathBuf,
    cue: Option<PathBuf>,
    destination: Option<PathBuf>,
    format: Format,
    edit: Edit,
    title_anchor: Option<usize>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    next_revision: u64,
    saved_revision: u64,
    playback: Option<Playback>,
    device: Option<String>,
    speed: f32,
    scrub_step: usize,
    announce_titles: bool,
    last_title: Option<usize>,
    device_items: Vec<(Option<String>, CheckMenuItem)>,
    speed_items: Vec<(f32, CheckMenuItem)>,
    session: Session,
    _open_document: OpenDocument,
    save: Option<Receiver<SaveMessage>>,
    save_selection: Option<Receiver<SelectionMessage>>,
    insert: Option<Receiver<Insertion>>,
    analyze: Option<Receiver<Result<String>>>,
}

#[derive(Clone, Copy)]
enum Command {
    Open,
    TrimArchive,
    Save,
    SaveAs,
    SaveSelection,
    Analyze,
    Close,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Duplicate,
    InsertFile,
    Delete,
    Crop,
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
    SpeedUp,
    SpeedDown,
    ResetSpeed,
    ZoomIn,
    ZoomOut,
    Preview,
    ReportPosition,
    ReportSelection,
    JumpTo,
    PreviousTitle,
    NextTitle,
    GoToTitle,
    ReadTitles,
    Status,
    Help,
}

pub fn choose_files(parent: Window, session: &Session) {
    let paths = FileDialog::new()
        .title("Open MP3 files")
        .filter("MP3 audio", &["mp3"])
        .open_many(Some(parent));
    for path in paths {
        open_file(parent, path, session);
    }
}

pub fn open_file(parent: Window, path: PathBuf, session: &Session) {
    let (sender, receiver) = mpsc::channel();
    let input = path.clone();
    let started = std::thread::Builder::new()
        .name("MP3 import".into())
        .spawn(move || {
            let result = fs::read(&input)
                .map_err(Into::into)
                .and_then(|bytes| mp3::scan(&bytes))
                .map(|scan| {
                    let cue_path = input.with_extension("cue");
                    let mut cue = None;
                    let mut cue_entries = Vec::new();
                    let mut cue_error = None;
                    match stream::read_cue(&cue_path) {
                        Ok(entries) => {
                            cue = Some(cue_path);
                            cue_entries = entries;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            cue_error =
                                Some(format!("Could not read {}: {error}", cue_path.display()));
                        }
                    }
                    Opened {
                        scan,
                        cue,
                        cue_entries,
                        cue_error,
                    }
                });
            let _ = sender.send(result);
        });
    if let Err(error) = started {
        show_error(
            parent,
            &format!("Could not open {}. {error}", path.display()),
        );
        return;
    }
    let session = session.clone();
    let mut document = Some(session.opening_document());
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
                Ok(opened) => {
                    if let Some(document) = document.take() {
                        create(path.clone(), opened, session.clone(), document);
                    }
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

fn same_path(left: &Path, right: &Path) -> bool {
    if let (Ok(left), Ok(right)) = (left.canonicalize(), right.canonicalize()) {
        return left == right;
    }
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn cue_message(outcome: &ExportOutcome) -> String {
    if let Some(cue) = &outcome.cue {
        format!(" CUE sheet: {}.", name(cue))
    } else if let Some(error) = &outcome.cue_error {
        format!(" The CUE sheet was not written. {error}")
    } else {
        String::new()
    }
}

fn saved_message(path: &Path, outcome: &ExportOutcome) -> String {
    format!("Saved {}.{}", name(path), cue_message(outcome))
}

fn export_with_cue(
    path: &Path,
    format: Format,
    frames: Frames,
    cue: Option<(PathBuf, Vec<(f64, String)>)>,
    cue_wanted: bool,
    replace_cue: bool,
) -> ExportResult {
    audio::export(path, format, frames).map_err(|error| error.to_string())?;
    let mut outcome = ExportOutcome {
        cue: None,
        cue_error: None,
    };
    if let Some((cue_path, entries)) = cue {
        match stream::write_cue_checked(&cue_path, &name(path), &entries, replace_cue) {
            Ok(()) => outcome.cue = Some(cue_path),
            Err(error) => {
                outcome.cue_error =
                    Some(format!("Could not write {}: {error}", cue_path.display()));
            }
        }
    } else if cue_wanted {
        outcome.cue_error = Some("No title markers remain, so no CUE sheet was written.".into());
    }
    Ok(outcome)
}

pub(crate) fn show_error(window: Window, error: &str) {
    message_box(
        Some(window),
        MessageKind::Error,
        Buttons::Ok,
        "DirectCut RS",
        error,
    );
}

// A multiline Win32 edit can claim Tab as input. Let the dialog-key handler
// move focus to the Close button while preserving the edit's other keys.
#[cfg(windows)]
fn enable_shortcut_tab_navigation(area: TextArea) {
    use windows::Win32::UI::WindowsAndMessaging::{MSG, WM_GETDLGCODE, WM_KEYDOWN};

    type SubclassProc = unsafe extern "system" fn(isize, u32, usize, isize, usize, usize) -> isize;
    #[link(name = "comctl32")]
    unsafe extern "system" {
        fn SetWindowSubclass(
            hwnd: isize,
            proc: Option<SubclassProc>,
            id: usize,
            data: usize,
        ) -> i32;
        fn DefSubclassProc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    }
    unsafe extern "system" fn edit_proc(
        hwnd: isize,
        msg: u32,
        wparam: usize,
        lparam: isize,
        _: usize,
        _: usize,
    ) -> isize {
        let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
        if msg == WM_GETDLGCODE && lparam != 0 {
            let key = unsafe { &*(lparam as *const MSG) };
            if key.message == WM_KEYDOWN && key.wParam.0 == 9 {
                return result & !0x0006; // DLGC_WANTTAB and DLGC_WANTALLKEYS
            }
        }
        result
    }
    if let Some(NativeHandle::Win32(hwnd)) = area.native_handle() {
        unsafe {
            SetWindowSubclass(hwnd as isize, Some(edit_proc), 1, 0);
        }
    }
}

#[cfg(not(windows))]
fn enable_shortcut_tab_navigation(_: TextArea) {}

fn show_shortcuts(text: &str) {
    let window = Window::new("Keyboard shortcuts");
    let column = VBox::new(window);
    column.set_padding(12);
    column.set_spacing(8);
    let area = TextArea::new(column);
    area.set_a11y_name("Keyboard shortcuts");
    area.set_read_only(true);
    area.set_min_size(600, 360);
    area.set_expand(1.0);
    area.set_text(text);
    enable_shortcut_tab_navigation(area);
    let close = Button::new(column, "&Close");
    close.on_click(move || window.close());
    window.on_cancel(move || window.close());
    window.set_size(700, 480);
    window.show();
    Timer::once(0, move || {
        if window.is_alive() && area.is_alive() {
            area.focus();
        }
    });
}

/// A read-only, selectable report window with a plain-text save command.
fn show_report(title: &str, note_text: &str, source: PathBuf, text: String) {
    let window = Window::new(title);
    let column = VBox::new(window);
    column.set_padding(12);
    column.set_spacing(8);
    let note = Label::new(column, note_text);
    let area = TextArea::new(column);
    area.set_a11y_name("Report");
    area.set_read_only(true);
    area.set_min_size(760, 460);
    area.set_expand(1.0);
    area.set_text(&text);
    let buttons = HBox::new(column);
    buttons.set_spacing(8);
    let save = Button::new(buttons, "&Save Report...");
    let close = Button::new(buttons, "&Close");
    save.on_click({
        let text = text.clone();
        let source = source.clone();
        move || {
            let suggested = report::suggested_name(&source, "analysis");
            let Some(path) = FileDialog::new()
                .title("Save analysis report")
                .filter("Text files", &["txt"])
                .file_name(&suggested)
                .save(Some(window))
            else {
                return;
            };
            match fs::write(&path, text.as_bytes()) {
                Ok(()) => note.set_text(&format!("Saved report to {}.", name(&path))),
                Err(error) => {
                    show_error(window, &format!("Could not save the report. {error}"));
                }
            }
        }
    });
    close.on_click(move || window.close());
    window.set_size(820, 560);
    window.show();
    area.focus();
}

fn create(path: PathBuf, opened: Opened, session: Session, open_document: OpenDocument) {
    let Opened {
        scan,
        cue,
        cue_entries,
        cue_error,
    } = opened;
    let window = Window::new("DirectCut RS");
    announce::attach(window);
    let column = VBox::new(window);
    column.set_padding(12);
    column.set_spacing(8);
    Label::new(column, "Timeline status (F6)");
    let status = TextArea::new(column);
    status.set_a11y_name("Timeline status");
    status.set_read_only(true);
    status.set_min_size(480, 125);
    let bar = MenuBar::new(window);
    let recovery = report::summary(&scan);
    let format = scan.format;
    let frames = scan.frames;
    let titles = cue_titles(cue_entries, format, frames.len());
    let activity = if let Some(error) = cue_error {
        format!("Opened MP3, but its CUE sheet could not be read. {error}")
    } else if cue.is_some() {
        format!("Opened MP3 with {} CUE title(s).", titles.len())
    } else {
        "Opened MP3.".into()
    };
    let editor = Rc::new(RefCell::new(Editor {
        window,
        status,
        activity,
        recovery,
        path,
        cue,
        destination: None,
        format,
        edit: Edit {
            frames,
            titles,
            cursor: 0,
            start: None,
            end: None,
            revision: 0,
        },
        title_anchor: None,
        undo: Vec::new(),
        redo: Vec::new(),
        next_revision: 1,
        saved_revision: 0,
        playback: None,
        device: None,
        speed: 1.0,
        scrub_step: 1,
        announce_titles: false,
        last_title: None,
        device_items: Vec::new(),
        speed_items: Vec::new(),
        session,
        _open_document: open_document,
        save: None,
        save_selection: None,
        insert: None,
        analyze: None,
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
    let add = |menu: Menu, label: &str, shortcut: &str, command: Command| {
        let item = MenuItem::new(menu, label);
        if !shortcut.is_empty() {
            item.set_accel(shortcut);
        }
        let editor = editor.clone();
        item.on_click(move || match command {
            Close => {
                if can_close(&editor) {
                    timer.stop();
                    window.close();
                }
            }
            Open => {
                let session = editor.borrow().session.clone();
                choose_files(window, &session);
            }
            TrimArchive => {
                let session = editor.borrow().session.clone();
                stream_window::trim_window(window, None, session);
            }
            _ => {
                let result = editor.borrow_mut().run(command);
                if let Err(error) = result {
                    show_error(window, &error.to_string());
                }
            }
        });
    };
    let file = Menu::new(bar, "&File");
    for &(label, shortcut, command) in &[
        ("&Open MP3...", "Ctrl+O", Open),
        ("&Trim Archive...", "", TrimArchive),
        ("&Save Edited MP3", "Ctrl+S", Save),
        ("Save Edited MP3 &As...", "Ctrl+Shift+S", SaveAs),
        ("Save Selection &As...", "", SaveSelection),
        ("Ana&lyze Source MP3...", "Ctrl+I", Analyze),
        ("&Close", "Ctrl+W", Close),
    ][..]
    {
        add(file, label, shortcut, command);
    }
    let edit = Menu::new(bar, "&Edit");
    for &(label, shortcut, command) in &[
        ("&Undo", "Ctrl+Z", Undo),
        ("&Redo", "Ctrl+Y", Redo),
        ("Cu&t", "Ctrl+X", Cut),
        ("&Copy", "Ctrl+C", Copy),
        ("&Paste", "Ctrl+V", Paste),
        ("D&uplicate Selection", "Ctrl+D", Duplicate),
        ("&Insert MP3 at Playhead...", "Insert", InsertFile),
        ("&Delete Selection", "Del", Delete),
        ("&Crop to Selection", "", Crop),
        ("Select &All", "Ctrl+A", SelectAll),
        ("C&lear Markers", "Esc", Clear),
    ][..]
    {
        add(edit, label, shortcut, command);
    }
    let navigate = Menu::new(bar, "&Navigate");
    for &(label, shortcut, command) in &[
        ("File &Start", "Ctrl+Home", Beginning),
        ("File &End", "Ctrl+End", End),
        ("File Start", "Shift+Home", Beginning),
        ("File End", "Shift+End", End),
        ("&Previous Frame", "Left", Move(-1, false)),
        ("&Next Frame", "Right", Move(1, false)),
        ("&Back One Second", "Ctrl+Left", Move(-1, true)),
        ("&Forward One Second", "Ctrl+Right", Move(1, true)),
        ("Scrub Zoom &In", "-", ZoomIn),
        ("Scrub Zoom &Out", "=", ZoomOut),
        ("Set Start &Marker", "[", StartMarker),
        ("Set E&nd Marker", "]", EndMarker),
        ("&Go to Start Marker", "Ctrl+[", GoStart),
        ("Go to End Mar&ker", "Ctrl+]", GoEnd),
        ("Repo&rt Position", "Ctrl+Shift+J", ReportPosition),
        (
            "Report Time Sele&ction",
            "Ctrl+Shift+Space",
            ReportSelection,
        ),
    ][..]
    {
        add(navigate, label, shortcut, command);
    }
    let titles = Menu::new(bar, "&Titles");
    for &(label, shortcut, command) in &[
        ("&Previous Title", "Ctrl+Shift+Left", PreviousTitle),
        ("&Next Title", "Ctrl+Shift+Right", NextTitle),
        ("&Go to Title...", "Ctrl+Shift+T", GoToTitle),
        ("&Read Title List", "", ReadTitles),
    ][..]
    {
        add(titles, label, shortcut, command);
    }
    let announce_titles = CheckMenuItem::new(titles, "&Announce Titles While Playing");
    {
        let editor = editor.clone();
        announce_titles.on_toggle(move |checked| {
            let mut editor = editor.borrow_mut();
            editor.announce_titles = checked;
            editor.last_title = None;
        });
    }
    let transport = Menu::new(bar, "&Transport");
    add(transport, "Play / &Stop", "Space", PlayStop);
    add(transport, "&Pause / Resume", "Ctrl+Space", PausePlay);
    add(transport, "P&review Selection", "Shift+Space", Preview);
    MenuSeparator::new(transport);
    add(transport, "&Jump to Time...", "Ctrl+J", JumpTo);
    MenuSeparator::new(transport);

    let device_menu = Menu::new(transport, "Playback &Device");
    let mut device_items = Vec::new();
    let default_device = CheckMenuItem::new(device_menu, "System &Default");
    default_device.set_checked(true);
    {
        let editor = editor.clone();
        default_device.on_toggle(move |checked| {
            if checked {
                if let Err(error) = editor.borrow_mut().select_device(None) {
                    show_error(window, &error.to_string());
                }
            } else {
                let checked = editor.borrow().device.is_none();
                default_device.set_checked(checked);
            }
        });
    }
    device_items.push((None, default_device));
    for name in audio::output_devices() {
        let item = CheckMenuItem::new(device_menu, &name.replace('&', "&&"));
        item.set_checked(editor.borrow().device.as_deref() == Some(name.as_str()));
        {
            let editor = editor.clone();
            let name = name.clone();
            item.on_toggle(move |checked| {
                if checked {
                    if let Err(error) = editor.borrow_mut().select_device(Some(name.clone())) {
                        show_error(window, &error.to_string());
                    }
                } else {
                    let checked = editor.borrow().device.as_deref() == Some(name.as_str());
                    item.set_checked(checked);
                }
            });
        }
        device_items.push((Some(name), item));
    }

    let speed_menu = Menu::new(transport, "Playback S&peed");
    add(speed_menu, "&Increase Speed", "Shift+=", SpeedUp);
    add(speed_menu, "&Decrease Speed", "Shift+-", SpeedDown);
    add(
        speed_menu,
        "&Reset Speed",
        "Ctrl+Shift+Backspace",
        ResetSpeed,
    );
    MenuSeparator::new(speed_menu);
    let mut speed_items = Vec::new();
    for speed in speed_steps() {
        let item = CheckMenuItem::new(speed_menu, &format!("{speed:.2}x"));
        item.set_checked((editor.borrow().speed - speed).abs() < 0.001);
        {
            let editor = editor.clone();
            item.on_toggle(move |checked| {
                if checked {
                    editor.borrow_mut().set_speed(speed);
                } else {
                    item.set_checked((editor.borrow().speed - speed).abs() < 0.001);
                }
            });
        }
        speed_items.push((speed, item));
    }
    MenuSeparator::new(transport);
    add(transport, "Read Timeline St&atus", "F6", Status);
    add(Menu::new(bar, "&Help"), "&Keyboard Shortcuts", "F1", Help);
    {
        let mut editor = editor.borrow_mut();
        editor.device_items = device_items;
        editor.speed_items = speed_items;
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
        (
            editor.window,
            editor.save.is_some() || editor.save_selection.is_some(),
            editor.dirty(),
        )
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
        let time = |frame| time_text(self.format, frame);
        let marker = |frame: Option<usize>| frame.map_or("unset".into(), time);
        let state = match &self.playback {
            Some(p) if p.sink.is_paused() => "Paused",
            Some(_) => "Playing",
            None => "Stopped",
        };
        let transport = if (self.speed - 1.0).abs() < 0.001 {
            state.to_string()
        } else {
            format!("{state} at {:.2}x speed", self.speed)
        };
        let titles = if self.edit.titles.is_empty() {
            "No CUE titles.".to_string()
        } else {
            match self.title_at(self.position()) {
                Some(index) => {
                    let marker = &self.edit.titles[index];
                    let end = self
                        .edit
                        .titles
                        .get(index + 1)
                        .map_or(self.edit.frames.len(), |next| next.frame);
                    format!(
                        "Title {} of {}: {}. Length {}.",
                        index + 1,
                        self.edit.titles.len(),
                        marker.title,
                        time(end.saturating_sub(marker.frame))
                    )
                }
                None => format!("Before title 1 of {}.", self.edit.titles.len()),
            }
        };
        format!(
            "{transport}. Position {} of {}. Scrub step {}.\r\nStart marker {}. End marker {}.\r\n{titles}\r\n{} Hz, {} {}. One timeline: paste inserts at the playhead and Delete closes the gap. {}\r\n{}{}{}",
            time(self.position()),
            time(self.edit.frames.len()),
            scrub_text(self.scrub_step, self.format),
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

    fn speak(&self, text: &str) {
        announce::announce(self.window, text);
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
            let snippet = p.snippet;
            self.playback = None;
            if let Some(error) = error {
                self.edit.cursor = position;
                self.activity = format!("Playback stopped. {error}");
            } else if !snippet {
                // Normal playback ran to the end; bounded playback such as a
                // scrub snippet or a selection preview keeps the playhead.
                self.edit.cursor = self.edit.frames.len();
            }
            self.refresh();
        }
        if self.announce_titles
            && self
                .playback
                .as_ref()
                .is_some_and(|p| !p.snippet && !p.sink.is_paused())
        {
            let position = self.position();
            let index = title_at_frame(&self.edit.titles, position);
            if index != self.last_title {
                self.last_title = index;
                if let Some(index) = index {
                    let marker = &self.edit.titles[index];
                    let message = format!(
                        "Title {} of {}: {}.",
                        index + 1,
                        self.edit.titles.len(),
                        marker.title
                    );
                    self.activity = message.clone();
                    self.speak(&message);
                }
            }
        } else if self.playback.is_none() {
            self.last_title = None;
        }
        if let Some(save) = &self.save {
            match save.try_recv() {
                Ok((path, revision, result)) => {
                    self.save = None;
                    match result {
                        Ok(outcome) => {
                            let message = saved_message(&path, &outcome);
                            self.activity = message.clone();
                            self.speak(&message);
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
        if let Some(selection) = &self.save_selection {
            match selection.try_recv() {
                Ok((path, result)) => {
                    self.save_selection = None;
                    match result {
                        Ok(outcome) => {
                            let message = format!(
                                "Saved the selection to {}.{}",
                                name(&path),
                                cue_message(&outcome)
                            );
                            self.activity = message.clone();
                            self.speak(&message);
                        }
                        Err(error) => {
                            self.activity = format!("Selection save failed. {error}");
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
                    self.save_selection = None;
                    self.activity = "Selection save failed. The export worker stopped.".into();
                    self.refresh();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(insert) = &self.insert {
            match insert.try_recv() {
                Ok(insertion) => {
                    self.insert = None;
                    match insertion.result {
                        Ok(scan) => {
                            if scan.format != self.format {
                                self.activity = format!(
                                    "Could not insert {}. Insert requires the same MPEG version, sample rate, and channel count.",
                                    name(&insertion.path)
                                );
                                let window = self.window;
                                let message = self.activity.clone();
                                App::post(move || {
                                    if window.is_alive() {
                                        show_error(window, &message);
                                    }
                                });
                            } else {
                                let count = scan.frames.len();
                                let gaps = scan.has_gaps();
                                let position = insertion.position.min(self.edit.frames.len());
                                self.replace(position..position, scan.frames);
                                self.activity = if gaps {
                                    format!(
                                        "Inserted {count} frames from {}. The source had gaps; retained frames were inserted without replacement audio.",
                                        name(&insertion.path)
                                    )
                                } else {
                                    format!(
                                        "Inserted {count} frames from {} at the playhead.",
                                        name(&insertion.path)
                                    )
                                };
                            }
                        }
                        Err(error) => {
                            self.activity =
                                format!("Could not insert {}. {error}", name(&insertion.path));
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
                    self.insert = None;
                    self.activity = "Insert failed. The import worker stopped.".into();
                    self.refresh();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(analysis) = &self.analyze {
            match analysis.try_recv() {
                Ok(result) => {
                    self.analyze = None;
                    match result {
                        Ok(text) => {
                            self.activity =
                                "Analysis complete. The source file was not modified.".into();
                            let title = format!("Analysis - {}", name(&self.path));
                            show_report(
                                &title,
                                "Read-only analysis. The source file was not modified.",
                                self.path.clone(),
                                text,
                            );
                        }
                        Err(error) => {
                            self.activity = format!("Analysis failed. {error}");
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
                    self.analyze = None;
                    self.activity = "Analysis failed. The analysis worker stopped.".into();
                    self.refresh();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    fn stop_here(&mut self) {
        self.edit.cursor = self.position();
        self.playback = None;
    }

    fn seek(&mut self, position: usize) -> Result<()> {
        // Any explicit seek cancels a title navigation anchor. Title jumps set
        // a new anchor after seeking.
        self.title_anchor = None;
        let playing = self.playback.as_ref().is_some_and(|p| !p.sink.is_paused());
        self.playback = None;
        self.edit.cursor = position.min(self.edit.frames.len());
        if playing && self.edit.cursor < self.edit.frames.len() {
            self.play()?;
        }
        Ok(())
    }

    /// Move the playhead and play a short snippet from the new position, the
    /// way a tape scrub sounds. Continuous playback stops, the snippet plays at
    /// normal speed whatever the speed setting is, and the playhead stays where
    /// the move put it when the snippet ends.
    fn scrub(&mut self, position: usize) -> Result<()> {
        let total = self.edit.frames.len();
        let position = position.min(total);
        self.playback = None;
        self.edit.cursor = position;
        self.title_anchor = None;
        if position >= total {
            return Ok(());
        }
        match Playback::start(
            self.format,
            &self.edit.frames,
            position,
            self.device.as_deref(),
            1.0,
            Some(scrub_snap_frames(self.format)),
        ) {
            Ok(scrub) => self.playback = Some(scrub),
            Err(error) => {
                self.playback = None;
                self.activity = format!(
                    "Scrubbing from {} produced no sound. {error}",
                    time_text(self.format, position)
                );
            }
        }
        Ok(())
    }

    /// Play exactly the marked range at the current speed, then leave the
    /// playhead at the range start for a replay or an edit.
    fn preview_selection(&mut self) -> Result<()> {
        let range = self.edit.selection()?;
        let end = range.end.min(self.edit.frames.len());
        let start = range.start.min(end);
        if start >= self.edit.frames.len() {
            return Err("The selection is past the end of the timeline.".into());
        }
        self.playback = None;
        self.edit.cursor = start;
        self.title_anchor = None;
        match Playback::start(
            self.format,
            &self.edit.frames,
            start,
            self.device.as_deref(),
            self.speed,
            Some(end - start),
        ) {
            Ok(preview) => self.playback = Some(preview),
            Err(error) => {
                self.playback = None;
                self.activity = format!("Preview produced no sound. {error}");
            }
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
            self.device.as_deref(),
            self.speed,
            None,
        )?);
        Ok(())
    }

    fn set_speed(&mut self, speed: f32) {
        self.speed = speed.clamp(SPEED_MIN, SPEED_MAX);
        if let Some(playback) = &self.playback {
            playback.set_speed(self.speed);
        }
        self.update_speed_checks();
        if (self.speed - 1.0).abs() < 0.001 {
            self.activity = "Playback speed 1.00x (normal pitch).".into();
        } else {
            self.activity = format!(
                "Playback speed {:.2}x. Speed changes pitch because this is vari-speed, not time stretch.",
                self.speed
            );
        }
        self.refresh();
    }

    fn change_speed(&mut self, delta: f32) {
        let speed = ((self.speed + delta) * 20.0).round() / 20.0;
        self.set_speed(speed);
    }

    fn select_device(&mut self, device: Option<String>) -> Result<()> {
        if self.device == device {
            self.update_device_checks();
            return Ok(());
        }
        let was_paused = self
            .playback
            .as_ref()
            .map(|playback| playback.sink.is_paused());
        let position = self.position();
        self.playback = None;
        self.device = device;
        self.update_device_checks();
        self.edit.cursor = position.min(self.edit.frames.len());
        if let Some(paused) = was_paused
            && self.edit.cursor < self.edit.frames.len()
        {
            self.play()?;
            if paused && let Some(playback) = &self.playback {
                playback.sink.pause();
            }
        }
        self.activity = format!("Playback device: {}.", self.device_name());
        self.refresh();
        Ok(())
    }

    fn device_name(&self) -> &str {
        self.device.as_deref().unwrap_or("System Default")
    }

    fn update_device_checks(&self) {
        for (device, item) in &self.device_items {
            item.set_checked(device.as_deref() == self.device.as_deref());
        }
    }

    fn update_speed_checks(&self) {
        for (speed, item) in &self.speed_items {
            item.set_checked((self.speed - speed).abs() < 0.001);
        }
    }

    fn replace(&mut self, range: std::ops::Range<usize>, insert: Frames) {
        self.stop_here();
        self.title_anchor = None;
        if self.undo.len() == 32 {
            self.undo.remove(0);
        }
        self.undo.push(self.edit.clone());
        self.redo.clear();
        let start = range.start;
        let removed = range.len();
        let inserted = insert.len();
        self.edit.frames.splice(range, insert);
        splice_titles(
            &mut self.edit.titles,
            start,
            start + removed,
            inserted,
            self.edit.frames.len(),
        );
        let end = start + inserted;
        self.edit.cursor = start;
        self.edit.start = (start != end).then_some(start);
        self.edit.end = (start != end).then_some(end);
        self.edit.revision = self.next_revision;
        self.next_revision += 1;
    }

    fn check_destination(&self, path: &Path) -> Result<bool> {
        if same_path(path, &self.path) {
            return Err(
                "Choose a different file name. DirectCut does not overwrite the opened source."
                    .into(),
            );
        }
        if self.cue.is_some()
            && same_path(
                &path.with_extension("cue"),
                &self.path.with_extension("cue"),
            )
        {
            return Err(
                "Choose a different file name. DirectCut does not overwrite the source CUE sheet."
                    .into(),
            );
        }
        let cue_exists = self.cue_plan(path, None).is_some() && path.with_extension("cue").exists();
        if cue_exists {
            let cue_path = path.with_extension("cue");
            if cue_exists
                && message_box(
                    Some(self.window),
                    MessageKind::Warning,
                    Buttons::OkCancel,
                    "Replace existing CUE sheet?",
                    &format!(
                        "Saving will replace the existing CUE sheet:\r\n{}\r\n\r\nContinue?",
                        cue_path.display()
                    ),
                ) != Answer::Ok
            {
                return Err(
                    "Save cancelled because the destination CUE sheet already exists.".into(),
                );
            }
        }
        Ok(cue_exists)
    }

    fn cue_plan(
        &self,
        path: &Path,
        range: Option<std::ops::Range<usize>>,
    ) -> Option<(PathBuf, Vec<(f64, String)>)> {
        self.cue.as_ref()?;
        let (titles, total) = match range {
            Some(range) => {
                let total = range.len();
                (titles_in_range(&self.edit.titles, &range), total)
            }
            None => (self.edit.titles.clone(), self.edit.frames.len()),
        };
        let entries = cue_entries(&titles, self.format, total);
        (!entries.is_empty()).then(|| (path.with_extension("cue"), entries))
    }

    fn save(&mut self, choose_path: bool) -> Result<()> {
        if self.save.is_some() || self.save_selection.is_some() {
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
        } else if let Some(destination) = self.destination.clone() {
            destination
        } else {
            return Err("Choose an output path for this save.".into());
        };
        let replace_cue = self.check_destination(&path)?;
        let format = self.format;
        let frames = self.edit.frames.clone();
        let cue_wanted = self.cue.is_some();
        let cue = self.cue_plan(&path, None);
        let revision = self.edit.revision;
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("MP3 export".into())
            .spawn(move || {
                let result = export_with_cue(&path, format, frames, cue, cue_wanted, replace_cue);
                let _ = sender.send((path, revision, result));
            })?;
        self.save = Some(receiver);
        self.activity = "Validating and saving MP3...".into();
        Ok(())
    }

    fn save_selection(&mut self) -> Result<()> {
        if self.save.is_some() || self.save_selection.is_some() {
            return Err("An MP3 is already being saved from this window.".into());
        }
        let range = self.edit.selection()?;
        let suggested = format!(
            "{} - selection.mp3",
            self.path
                .file_stem()
                .unwrap_or(self.path.as_os_str())
                .to_string_lossy()
        );
        let Some(path) = FileDialog::new()
            .title("Save selection as MP3")
            .filter("MP3 audio", &["mp3"])
            .file_name(&suggested)
            .save(Some(self.window))
        else {
            return Ok(());
        };
        let replace_cue = self.check_destination(&path)?;
        let format = self.format;
        let frames = self.edit.frames[range.clone()].to_vec();
        let cue_wanted = self.cue.is_some();
        let cue = self.cue_plan(&path, Some(range));
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("MP3 selection export".into())
            .spawn(move || {
                let result = export_with_cue(&path, format, frames, cue, cue_wanted, replace_cue);
                let _ = sender.send((path, result));
            })?;
        self.save_selection = Some(receiver);
        self.activity = "Validating and saving the selection...".into();
        Ok(())
    }

    fn insert_file(&mut self) -> Result<()> {
        if self.insert.is_some() {
            return Err("An MP3 is already being read for insertion.".into());
        }
        let Some(path) = FileDialog::new()
            .title("Insert MP3 at the playhead")
            .filter("MP3 audio", &["mp3"])
            .open(Some(self.window))
        else {
            return Ok(());
        };
        self.stop_here();
        let position = self.position();
        let input = path.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("MP3 insert".into())
            .spawn(move || {
                let result: Result<mp3::Scan> = fs::read(&input)
                    .map_err(Into::into)
                    .and_then(|bytes| mp3::scan(&bytes));
                let _ = sender.send(Insertion {
                    path: input,
                    position,
                    result,
                });
            })?;
        self.insert = Some(receiver);
        self.activity = format!("Reading {} for insertion...", name(&path));
        Ok(())
    }

    fn duplicate(&mut self) -> Result<()> {
        let range = self.edit.selection()?;
        let copy = self.edit.frames[range.clone()].to_vec();
        let count = copy.len();
        self.replace(range.end..range.end, copy);
        self.activity = format!("Duplicated {count} frames after the selection.");
        Ok(())
    }

    fn crop(&mut self) -> Result<()> {
        let total = self.edit.frames.len();
        let range = self.edit.selection()?;
        if range.start == 0 && range.end == total {
            self.activity = "The selection already covers the timeline.".into();
            return Ok(());
        }
        let titles = titles_in_range(&self.edit.titles, &range);
        let frames = self.edit.frames[range].to_vec();
        self.replace(0..total, frames);
        self.edit.titles = titles;
        self.edit.start = None;
        self.edit.end = None;
        self.activity = "Cropped the timeline to the selection.".into();
        Ok(())
    }

    fn analyze(&mut self) -> Result<()> {
        if self.analyze.is_some() {
            return Err("The source is already being analyzed.".into());
        }
        let path = self.path.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("MP3 analysis".into())
            .spawn(move || {
                let result = report::analyze_file(&path);
                let _ = sender.send(result);
            })?;
        self.analyze = Some(receiver);
        self.activity = "Analyzing the source MP3 (read-only)...".into();
        Ok(())
    }

    fn jump_to(&mut self) -> Result<()> {
        let Some(text) = prompt(
            Some(self.window),
            "Jump to time",
            "Enter a time as ss, mm:ss, or hh:mm:ss. To jump to a CUE title, enter t and the title number, for example t24.",
            "",
        ) else {
            return Ok(());
        };
        match parse_jump(&text)? {
            JumpTarget::Title(number) => {
                let index = self.title_index(number)?;
                let frame = self.edit.titles[index].frame;
                self.seek(frame)?;
                self.title_anchor = Some(frame);
                let message = self.title_text(index);
                self.activity = message.clone();
                self.speak(&message);
            }
            JumpTarget::Time(seconds) => {
                let target =
                    (seconds * f64::from(self.format.rate) / self.format.samples() as f64).round();
                if !target.is_finite() || target > usize::MAX as f64 {
                    return Err("That time is too large.".into());
                }
                let target = target as usize;
                let end = self.edit.frames.len();
                let message = if target >= end {
                    self.seek(end)?;
                    "Jumped to the end; the requested time is past the end.".to_string()
                } else {
                    self.seek(target)?;
                    format!("Jumped to {}.", time_text(self.format, target))
                };
                self.activity = message.clone();
                self.speak(&message);
            }
        }
        Ok(())
    }

    fn title_index(&self, number: usize) -> Result<usize> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        if number == 0 {
            return Err("Title numbers start at 1.".into());
        }
        number
            .checked_sub(1)
            .filter(|&index| index < self.edit.titles.len())
            .ok_or_else(|| {
                format!(
                    "Title {number} does not exist. This file has {} title(s).",
                    self.edit.titles.len()
                )
                .into()
            })
    }

    fn title_at(&self, frame: usize) -> Option<usize> {
        title_at_frame(&self.edit.titles, frame)
    }

    fn title_text(&self, index: usize) -> String {
        let marker = &self.edit.titles[index];
        let end = self
            .edit
            .titles
            .get(index + 1)
            .map_or(self.edit.frames.len(), |next| next.frame);
        format!(
            "Title {} of {}: {}. Starts at {}. Length {}.",
            index + 1,
            self.edit.titles.len(),
            marker.title,
            time_text(self.format, marker.frame),
            time_text(self.format, end.saturating_sub(marker.frame))
        )
    }

    fn next_title(&mut self) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let position = self.position();
        let total = self.edit.frames.len();
        match next_title_index(&self.edit.titles, position, total) {
            Some(index) => {
                let frame = self.edit.titles[index].frame;
                self.seek(frame)?;
                self.title_anchor = Some(frame);
                let message = self.title_text(index);
                self.activity = message.clone();
                self.speak(&message);
            }
            None => {
                self.seek(total)?;
                let message = "End of timeline. No next title.";
                self.activity = message.into();
                self.speak(message);
            }
        }
        Ok(())
    }

    fn previous_title(&mut self) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let position = self.position();
        let total = self.edit.frames.len();
        match previous_title_index(&self.edit.titles, position, total) {
            Some(index) => {
                let frame = self.edit.titles[index].frame;
                self.seek(frame)?;
                self.title_anchor = Some(frame);
                let message = self.title_text(index);
                self.activity = message.clone();
                self.speak(&message);
            }
            None => {
                self.seek(0)?;
                let message = "Start of timeline. No previous title.";
                self.activity = message.into();
                self.speak(message);
            }
        }
        Ok(())
    }

    fn find_title(&self, query: &str) -> Result<usize> {
        let query = query.trim();
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        if query.is_empty() {
            return Err("Enter a title number or part of a title name.".into());
        }
        let numbered = if query.len() > 1 && matches!(query.as_bytes()[0], b't' | b'T') {
            &query[1..]
        } else {
            query
        };
        if let Ok(number) = numbered.trim().parse::<usize>() {
            return self.title_index(number);
        }
        let needle = query.to_lowercase();
        self.edit
            .titles
            .iter()
            .position(|marker| marker.title.to_lowercase().contains(&needle))
            .ok_or_else(|| format!("No title contains \"{query}\".").into())
    }

    fn go_to_title(&mut self) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let Some(text) = prompt(
            Some(self.window),
            "Go to title",
            "Enter a title number, t and a number, or part of a title name.",
            "",
        ) else {
            return Ok(());
        };
        let index = self.find_title(&text)?;
        let frame = self.edit.titles[index].frame;
        self.seek(frame)?;
        self.title_anchor = Some(frame);
        let message = self.title_text(index);
        self.activity = message.clone();
        self.speak(&message);
        Ok(())
    }

    fn read_titles(&self) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let total = self.edit.frames.len();
        let mut text = format!(
            "{} CUE title(s). Times start from the timeline start.\r\n\r\n",
            self.edit.titles.len()
        );
        for (index, marker) in self.edit.titles.iter().enumerate() {
            let end = self
                .edit
                .titles
                .get(index + 1)
                .map_or(total, |next| next.frame);
            text.push_str(&format!(
                "{}. {}  Start {}  Length {}\r\n",
                index + 1,
                marker.title,
                time_text(self.format, marker.frame),
                time_text(self.format, end.saturating_sub(marker.frame))
            ));
        }
        let title = format!("CUE titles - {}", name(&self.path));
        show_report(
            &title,
            "Read-only title list. The source file was not modified.",
            self.path.clone(),
            text,
        );
        Ok(())
    }

    /// Resolve the frame for a marker command. Right after a title jump the
    /// playhead has already moved past the title start, so use the navigation
    /// anchor; otherwise use the live playhead position.
    fn marker_position(&mut self) -> usize {
        let position = self.position();
        let anchor = self.title_anchor.take();
        marker_frame(&self.edit.titles, self.format, anchor, position)
    }

    fn mark_start(&mut self) {
        let frame = self.marker_position();
        let cleared = self.edit.end.is_some_and(|end| end <= frame);
        if cleared {
            self.edit.end = None;
        }
        self.edit.start = Some(frame);
        let mut message = format!("Start marker set at {}.", time_text(self.format, frame));
        match self.edit.end {
            Some(end) if end > frame => message.push_str(&format!(
                " Selection length {}.",
                time_text(self.format, end - frame)
            )),
            _ if cleared => message.push_str(" The end marker was cleared."),
            _ => message.push_str(" Set an end marker after it."),
        }
        self.activity = message;
        self.speak(&self.activity);
    }

    fn mark_end(&mut self) {
        let frame = self.marker_position();
        let cleared = self.edit.start.is_some_and(|start| start >= frame);
        if cleared {
            self.edit.start = None;
        }
        self.edit.end = Some(frame);
        let mut message = format!("End marker set at {}.", time_text(self.format, frame));
        match self.edit.start {
            Some(start) if start < frame => message.push_str(&format!(
                " Selection length {}.",
                time_text(self.format, frame - start)
            )),
            _ if cleared => {
                message.push_str(" The start marker was cleared. Set a start marker before it.")
            }
            _ => message.push_str(" Set a start marker before it."),
        }
        self.activity = message;
        self.speak(&self.activity);
    }

    /// Speak the playhead position and current title on demand, the way
    /// OSARA's cursor report works, without a modal dialog.
    fn report_position(&mut self) {
        let mut message = format!(
            "Position {} of {}.",
            time_text(self.format, self.position()),
            time_text(self.format, self.edit.frames.len())
        );
        if !self.edit.titles.is_empty() {
            match self.title_at(self.position()) {
                Some(index) => message.push_str(&format!(
                    " Title {} of {}: {}.",
                    index + 1,
                    self.edit.titles.len(),
                    self.edit.titles[index].title
                )),
                None => {
                    message.push_str(&format!(" Before title 1 of {}.", self.edit.titles.len()))
                }
            }
        }
        self.activity = message;
        self.speak(&self.activity);
    }

    /// Speak the time selection on demand: its start, end, and length, or
    /// which marker is still missing.
    fn report_selection(&mut self) {
        let message = match (self.edit.start, self.edit.end) {
            (Some(start), Some(end)) if start < end => format!(
                "Selection from {} to {}, length {}.",
                time_text(self.format, start),
                time_text(self.format, end),
                time_text(self.format, end - start)
            ),
            (Some(start), _) => format!(
                "Start marker {}. Set an end marker after it.",
                time_text(self.format, start)
            ),
            (_, Some(end)) => format!(
                "End marker {}. Set a start marker before it.",
                time_text(self.format, end)
            ),
            _ => "No time selection.".into(),
        };
        self.activity = message;
        self.speak(&self.activity);
    }

    fn run(&mut self, command: Command) -> Result<()> {
        use Command::*;
        match command {
            Open | TrimArchive | Close => unreachable!(),
            Save | SaveAs => self.save(matches!(command, SaveAs))?,
            SaveSelection => self.save_selection()?,
            Analyze => self.analyze()?,
            InsertFile => self.insert_file()?,
            Duplicate => self.duplicate()?,
            Crop => self.crop()?,
            Beginning => self.seek(0)?,
            End => self.seek(self.edit.frames.len())?,
            Move(delta, seconds) => {
                let step = if seconds {
                    frames_per_second(self.format)
                } else {
                    self.scrub_step
                };
                let target = self
                    .position()
                    .saturating_add_signed(delta as isize * step as isize);
                self.scrub(target)?;
            }
            StartMarker => self.mark_start(),
            EndMarker => self.mark_end(),
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
                let length = time_text(self.format, range.len());
                if !matches!(command, Delete) {
                    *self.session.clipboard.borrow_mut() =
                        Some((self.format, self.edit.frames[range.clone()].to_vec()));
                    self.activity =
                        format!("Copied selection of {length} to the DirectCut clipboard.");
                    if matches!(command, Copy) {
                        let message = self.activity.clone();
                        self.speak(&message);
                    }
                }
                if !matches!(command, Copy) {
                    self.replace(range, Vec::new());
                    self.activity = if matches!(command, Cut) {
                        format!("Cut selection of {length}. Later audio moved earlier.")
                    } else {
                        format!("Deleted selection of {length}. Later audio moved earlier.")
                    };
                    let message = self.activity.clone();
                    self.speak(&message);
                }
            }
            Paste => {
                let (format, frames) = self
                    .session
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
                self.activity = "Pasted audio at the playhead.".into();
            }
            Undo => {
                if let Some(edit) = self.undo.pop() {
                    self.stop_here();
                    self.title_anchor = None;
                    self.redo.push(std::mem::replace(&mut self.edit, edit));
                }
            }
            Redo => {
                if let Some(edit) = self.redo.pop() {
                    self.stop_here();
                    self.title_anchor = None;
                    self.undo.push(std::mem::replace(&mut self.edit, edit));
                }
            }
            PlayStop => {
                match self.playback.as_ref().map(|p| p.sink.is_paused()) {
                    // Space resumes a pause from the paused position, the way
                    // Reaper's space bar does.
                    Some(true) => {
                        if let Some(p) = &self.playback {
                            p.sink.play();
                        }
                    }
                    Some(false) => {
                        if let Some(p) = self.playback.take() {
                            self.edit.cursor = p.stop_return;
                        }
                    }
                    None => self.play()?,
                }
            }
            PausePlay => {
                match self.playback.as_ref().map(|p| p.sink.is_paused()) {
                    Some(true) => {
                        if let Some(p) = &self.playback {
                            p.sink.play();
                        }
                    }
                    Some(false) => {
                        // Pausing snaps the playhead to the paused position,
                        // like Reaper's pause key drops the edit cursor.
                        let position = self.position();
                        if let Some(p) = &mut self.playback {
                            p.sink.pause();
                            p.stop_return = position;
                        }
                        self.edit.cursor = position;
                    }
                    None => self.play()?,
                }
            }
            Preview => self.preview_selection()?,
            SpeedUp => self.change_speed(0.25),
            SpeedDown => self.change_speed(-0.25),
            ResetSpeed => self.set_speed(1.0),
            ZoomIn | ZoomOut => {
                let zoom_in = matches!(command, ZoomIn);
                let before = self.scrub_step;
                let zoomed = zoomed_scrub_step(before, zoom_in);
                self.scrub_step = if zoom_in {
                    zoomed
                } else {
                    zoomed.min(frames_per_second(self.format))
                };
                self.activity = if self.scrub_step == before {
                    if zoom_in {
                        "Scrub step is already 1 frame.".into()
                    } else {
                        "Scrub step is already 1 second.".into()
                    }
                } else {
                    format!("Scrub step {}.", scrub_text(self.scrub_step, self.format))
                };
                let message = self.activity.clone();
                self.speak(&message);
            }
            JumpTo => self.jump_to()?,
            ReportPosition => self.report_position(),
            ReportSelection => self.report_selection(),
            PreviousTitle => self.previous_title()?,
            NextTitle => self.next_title()?,
            GoToTitle => self.go_to_title()?,
            ReadTitles => self.read_titles()?,
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
                show_shortcuts(
                    "Space plays, resumes a pause, or stops back at the playback start. Ctrl+Space pauses or resumes, and pausing leaves the playhead at the pause position.\r\n\
                     Shift+Space previews the marked range and stops.\r\n\
                     Ctrl+Shift+J reports the position and current title. Ctrl+Shift+Space reports the time selection.\r\n\
                     Left and Right scrub by the current step, 1 frame by default, and Ctrl+Left and Ctrl+Right scrub about a second. Each move stops playback and plays a short snippet from the new position.\r\n\
                     Minus zooms the scrub step in and Equals zooms it out; zooming out doubles the step up to one second.\r\n\
                     Ctrl+Home and Ctrl+End, or Shift+Home and Shift+End, go to the file boundaries.\r\n\
                     Ctrl+J jumps to a time from the timeline start, typed as ss, mm:ss, or hh:mm:ss, or to a CUE title typed as t and the number, such as t24.\r\n\
                     Ctrl+Shift+Left and Ctrl+Shift+Right move to the previous or next CUE title. Ctrl+Shift+T goes to a title by number or name.\r\n\
                     Titles > Read Title List shows each title start and its derived length. Announce Titles While Playing in the Titles menu speaks each title as playback enters it.\r\n\
                     Title jumps, marker positions, position and selection reports, title announcements while playing, and completed saves are announced through the screen reader when one is running. No speech is produced otherwise.\r\n\
                     Shift+Plus and Shift+Minus change playback speed from 0.25x to 4.00x in 0.25x steps. Ctrl+Shift+Backspace resets it to 1.00x. Speed changes pitch.\r\n\
                     Playback Device and Playback Speed are submenus of the Transport menu; the device applies to this window.\r\n\
                     [ sets the start marker and ] sets the end marker. A marker set right after a title jump marks that title's start, and a marker at or past the other one clears it. Escape clears both.\r\n\
                     Ctrl+X cuts, Ctrl+C copies, and Ctrl+V pastes at the playhead.\r\n\
                     Ctrl+D duplicates the selection. Insert inserts another MP3 at the playhead.\r\n\
                     Delete removes the selection and closes the gap. Crop to Selection keeps only the marked range.\r\n\
                     Save Selection As exports the marked range without changing the timeline.\r\n\
                     This is one timeline, so edits always ripple; paste inserts and delete closes the gap.\r\n\
                     Ctrl+Z undoes and Ctrl+Y redoes edits. Ctrl+S saves without overwriting the opened source.\r\n\
                     Ctrl+I analyzes the source MP3 without changing it.\r\n\
                     File > Trim Archive opens the CUE-based archive trimmer.\r\n\
                     F6 reads the timeline status. All commands are also in the menus.",
                );
            }
        }
        self.refresh();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Format, JumpTarget, TitleMarker, cue_entries, cue_titles, frames_per_second, marker_frame,
        next_title_index, parse_jump, parse_timestamp, previous_title_index, scrub_snap_frames,
        scrub_text, speed_steps, splice_titles, title_at_frame, titles_in_range, zoomed_scrub_step,
    };

    fn marker(frame: usize, title: &str) -> TitleMarker {
        TitleMarker {
            frame,
            title: title.into(),
        }
    }

    #[test]
    fn parses_seconds_minutes_and_hours() {
        assert_eq!(parse_timestamp("5").unwrap(), 5.0);
        assert_eq!(parse_timestamp("5.5").unwrap(), 5.5);
        assert_eq!(parse_timestamp("2:03").unwrap(), 123.0);
        assert_eq!(parse_timestamp("1:02:03").unwrap(), 3723.0);
        assert_eq!(parse_timestamp("90").unwrap(), 90.0);
        assert_eq!(parse_timestamp("90:00").unwrap(), 5400.0);
        assert_eq!(parse_timestamp(" 1 : 02 : 03.5 ").unwrap(), 3723.5);
    }

    #[test]
    fn rejects_malformed_times() {
        for text in ["", ":", "1:", ":2", "1:2:3:4", "one", "-1", "1:-2"] {
            assert!(
                parse_timestamp(text).is_err(),
                "{text:?} should be rejected"
            );
        }
        assert!(parse_timestamp("1:60").is_err());
        assert!(parse_timestamp("1:60:00").is_err());
        assert!(parse_timestamp("1:00:60").is_err());
    }

    #[test]
    fn parses_title_jumps() {
        assert_eq!(parse_jump("t24").unwrap(), JumpTarget::Title(24));
        assert_eq!(parse_jump(" T1 ").unwrap(), JumpTarget::Title(1));
        assert_eq!(parse_jump("24").unwrap(), JumpTarget::Time(24.0));
        assert_eq!(parse_jump("1:02:03").unwrap(), JumpTarget::Time(3723.0));
        for text in ["t0", "t", "t2:03", "t-1"] {
            assert!(parse_jump(text).is_err(), "{text:?} should be rejected");
        }
    }

    #[test]
    fn cue_titles_snap_to_frames_and_keep_the_later_duplicate() {
        let format = Format::test(44_100, 2);
        let frame = format.samples() as f64 / f64::from(format.rate);
        let entries = vec![
            (0.0, "A".into()),
            (frame * 5.0 + 0.000_001, "B".into()),
            (frame * 5.0, "C".into()),
            (frame * 10.0, "Past end".into()),
        ];
        assert_eq!(
            cue_titles(entries, format, 10),
            vec![marker(0, "A"), marker(5, "C")]
        );
    }

    #[test]
    fn splice_titles_shifts_later_titles_and_drops_enclosed_ones() {
        let mut titles = vec![
            marker(0, "A"),
            marker(5, "B"),
            marker(10, "C"),
            marker(15, "D"),
        ];
        splice_titles(&mut titles, 5, 15, 0, 10);
        assert_eq!(titles, vec![marker(0, "A"), marker(5, "D")]);

        let mut titles = vec![marker(0, "A"), marker(10, "B")];
        splice_titles(&mut titles, 5, 5, 3, 23);
        assert_eq!(titles, vec![marker(0, "A"), marker(13, "B")]);

        let mut titles = vec![marker(0, "A"), marker(10, "B")];
        splice_titles(&mut titles, 10, 10, 3, 23);
        assert_eq!(titles, vec![marker(0, "A"), marker(10, "B")]);

        let mut titles = vec![marker(0, "A"), marker(10, "B"), marker(20, "C")];
        splice_titles(&mut titles, 8, 12, 0, 30);
        assert_eq!(titles, vec![marker(0, "A"), marker(16, "C")]);
    }

    #[test]
    fn crop_titles_keep_intersecting_titles_and_synthesize_the_holder() {
        let titles = vec![marker(0, "A"), marker(10, "B"), marker(20, "C")];
        assert_eq!(
            titles_in_range(&titles, &(5..25)),
            vec![marker(0, "A"), marker(5, "B"), marker(15, "C")]
        );
        assert_eq!(
            titles_in_range(&titles, &(10..25)),
            vec![marker(0, "B"), marker(10, "C")]
        );
        assert_eq!(titles_in_range(&titles, &(10..12)), vec![marker(0, "B")]);
    }

    #[test]
    fn title_navigation_is_relative_to_the_containing_title() {
        let titles = vec![marker(0, "A"), marker(10, "B"), marker(20, "C")];
        // Next skips the containing title, including from its exact start.
        assert_eq!(next_title_index(&titles, 0, 30), Some(1));
        assert_eq!(next_title_index(&titles, 5, 30), Some(1));
        assert_eq!(next_title_index(&titles, 10, 30), Some(2));
        assert_eq!(next_title_index(&titles, 25, 30), None);
        assert_eq!(next_title_index(&titles, 30, 30), None);
        // Previous goes to the title before the containing one, even when
        // playback has already moved the playhead past a title start.
        assert_eq!(previous_title_index(&titles, 0, 30), None);
        assert_eq!(previous_title_index(&titles, 5, 30), None);
        assert_eq!(previous_title_index(&titles, 10, 30), Some(0));
        assert_eq!(previous_title_index(&titles, 15, 30), Some(0));
        assert_eq!(previous_title_index(&titles, 25, 30), Some(1));
        // At the end of the timeline, previous reaches the last title.
        assert_eq!(previous_title_index(&titles, 30, 30), Some(2));
        // Titles do not have to start at frame 0.
        let late = vec![marker(5, "A"), marker(15, "B")];
        assert_eq!(next_title_index(&late, 0, 30), Some(0));
        assert_eq!(previous_title_index(&late, 0, 30), None);
        assert_eq!(previous_title_index(&late, 5, 30), None);
        // Inside the last title: previous is the title before it.
        assert_eq!(previous_title_index(&late, 29, 30), Some(0));
        // At the end of the timeline: previous is the last title.
        assert_eq!(previous_title_index(&late, 30, 30), Some(1));
        assert_eq!(title_at_frame(&late, 4), None);
        assert_eq!(title_at_frame(&late, 5), Some(0));
        assert_eq!(title_at_frame(&late, 30), Some(1));
    }

    #[test]
    fn markers_snap_to_a_recent_title_jump() {
        let format = Format::test(44_100, 2);
        let titles = vec![marker(0, "A"), marker(100, "B"), marker(2000, "C")];
        let second = (f64::from(format.rate) / format.samples() as f64).round() as usize;
        // Still inside the jumped-to title and within the snap window.
        assert_eq!(
            marker_frame(&titles, format, Some(100), 100 + second / 4),
            100
        );
        // Same title, but too long after the jump: no snap.
        assert_eq!(
            marker_frame(&titles, format, Some(100), 100 + 3 * second),
            100 + 3 * second
        );
        // Playback has moved into a later title: no snap.
        assert_eq!(marker_frame(&titles, format, Some(100), 2100), 2100);
        // No anchor: use the playhead.
        assert_eq!(marker_frame(&titles, format, None, 105), 105);
    }

    #[test]
    fn scrub_zoom_halves_and_doubles_within_limits() {
        let format = Format::test(44_100, 2);
        let max = frames_per_second(format);
        assert_eq!(max, 39);
        // Zooming in halves and stops at one frame.
        assert_eq!(zoomed_scrub_step(16, true), 8);
        assert_eq!(zoomed_scrub_step(2, true), 1);
        assert_eq!(zoomed_scrub_step(1, true), 1);
        // Zooming out doubles and stops at one second of frames.
        assert_eq!(zoomed_scrub_step(1, false), 2);
        assert_eq!(zoomed_scrub_step(32, false).min(max), max);
        assert_eq!(zoomed_scrub_step(max, false).min(max), max);
    }

    #[test]
    fn scrub_text_reports_frames_and_milliseconds() {
        let format = Format::test(44_100, 2);
        assert_eq!(scrub_text(1, format), "1 frame (26 ms)");
        assert_eq!(scrub_text(2, format), "2 frames (52 ms)");
        assert_eq!(
            scrub_text(frames_per_second(format), format),
            "39 frames (1019 ms)"
        );
    }

    #[test]
    fn scrub_snippet_is_about_a_tenth_of_a_second() {
        let format = Format::test(44_100, 2);
        assert_eq!(scrub_snap_frames(format), 4);
        // Slow formats still get at least one frame.
        assert_eq!(scrub_snap_frames(Format::test(8_000, 1)), 1);
    }

    #[test]
    fn speed_steps_cover_the_varispeed_range() {
        let steps: Vec<f32> = speed_steps().collect();
        assert_eq!(steps.len(), 16);
        assert_eq!(steps[0], 0.25);
        assert_eq!(steps[8], 2.25);
        assert_eq!(steps[15], 4.0);
    }

    #[test]
    fn cue_entries_use_derived_start_times() {
        let format = Format::test(44_100, 2);
        let frame = format.samples() as f64 / f64::from(format.rate);
        let titles = vec![marker(0, "A"), marker(10, "B")];
        let entries = cue_entries(&titles, format, 20);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], (0.0, "A".into()));
        assert!((entries[1].0 - frame * 10.0).abs() < 1e-9);
        assert_eq!(entries[1].1, "B");
    }
}
