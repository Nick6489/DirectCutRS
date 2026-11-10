#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod announce;
mod audio;
mod editor;
mod mp3;
mod paths;
mod report;
mod stream;
mod stream_window;
#[cfg(windows)]
mod updates;

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    process::ExitCode,
    rc::Rc,
};

use mp3::Frames;
use rungui::*;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    #[cfg(windows)]
    if args.first().is_some_and(|arg| arg == "--freshen-apply") {
        return updates::run_helper(args);
    }
    if args.first().is_some_and(|arg| arg == "--analyze") {
        return analyze(&args[1..]);
    }

    let app = App::new("DirectCut RS").expect("Could not initialize DirectCut RS");
    let session = Session::default();
    let start = Window::new("DirectCut RS");
    let layout = VBox::new(start);
    layout.set_padding(16);
    layout.set_spacing(12);
    let open = Button::new(layout, "&Open MP3...");
    let about = Button::new(layout, "&About...");

    #[cfg(windows)]
    let checks = {
        let checks = Rc::new(updates::Checks::new(
            start,
            open,
            about,
            session.documents.clone(),
        ));
        about.on_click({
            let checks = checks.clone();
            move || checks.about()
        });
        checks
    };
    #[cfg(not(windows))]
    about.on_click(move || {
        message_box(Some(start), MessageKind::Info, Buttons::Ok, "About DirectCut RS",
            "DirectCut RS 1.1\nBy Nick Giannak III\nContributions by Derek Lane.\nMade exclusively with artificial intelligence.\nCopyright © 2026 Nick Giannak III\nMIT License");
    });
    open.on_click({
        let session = session.clone();
        #[cfg(windows)]
        let checks = checks.clone();
        move || {
            #[cfg(windows)]
            checks.file_picker_open.set(true);
            editor::choose_files(start, &session);
            #[cfg(windows)]
            checks.file_picker_open.set(false);
        }
    });
    let bar = MenuBar::new(start);
    let file_menu = Menu::new(bar, "&File");
    let menu_open = MenuItem::new(file_menu, "&Open MP3...");
    menu_open.set_accel("Ctrl+O");
    let menu_trim = MenuItem::new(file_menu, "&Trim Archive...");
    MenuSeparator::new(file_menu);
    let menu_exit = MenuItem::new(file_menu, "E&xit");
    menu_open.on_click({
        let session = session.clone();
        #[cfg(windows)]
        let checks = checks.clone();
        move || {
            #[cfg(windows)]
            checks.file_picker_open.set(true);
            editor::choose_files(start, &session);
            #[cfg(windows)]
            checks.file_picker_open.set(false);
        }
    });
    menu_trim.on_click({
        let session = session.clone();
        move || stream_window::trim_window(start, None, session.clone())
    });
    menu_exit.on_click(move || start.close());

    start.set_size(420, 160);
    start.show();
    open.focus();

    #[cfg(windows)]
    Timer::once(updates::CONFIRM_DELAY_MS, move || {
        if start.is_alive() {
            updates::confirm_startup(start);
            checks.start(false);
        }
    });

    for path in args.into_iter().map(PathBuf::from) {
        editor::open_file(start, path, &session);
    }
    app.run();
    ExitCode::SUCCESS
}

fn analyze(args: &[std::ffi::OsString]) -> ExitCode {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut force = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--out") => match args.next() {
                Some(path) => output = Some(PathBuf::from(path.as_os_str())),
                None => {
                    eprintln!("--out needs a report path.");
                    return ExitCode::FAILURE;
                }
            },
            Some("--force") => force = true,
            Some("-h" | "--help") => {
                print_analysis_usage();
                return ExitCode::SUCCESS;
            }
            _ if input.is_none() => input = Some(PathBuf::from(arg.as_os_str())),
            _ => {
                eprintln!("Only one input file can be analyzed at a time.");
                print_analysis_usage();
                return ExitCode::FAILURE;
            }
        }
    }
    let Some(input) = input else {
        print_analysis_usage();
        return ExitCode::FAILURE;
    };
    let text = match report::analyze_file(&input) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("Could not analyze {}: {error}", input.display());
            return ExitCode::FAILURE;
        }
    };
    let Some(output) = output else {
        print!("{text}");
        return ExitCode::SUCCESS;
    };
    if same_file(&input, &output) {
        eprintln!("Refusing to write the report over the input file.");
        return ExitCode::FAILURE;
    }
    if output.exists() && !force {
        eprintln!(
            "{} already exists. Pass --force to replace it.",
            output.display()
        );
        return ExitCode::FAILURE;
    }
    if let Err(error) = std::fs::write(&output, text.as_bytes()) {
        eprintln!("Could not write {}: {error}", output.display());
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn print_analysis_usage() {
    eprintln!(
        "DirectCut RS archive analysis\r\n\
         Usage: directcut-rs --analyze <input.mp3> [--out <report.txt> [--force]]\r\n\
         The input is read only and is never modified. Without --out, the report is\r\n\
         printed to the console (visible when started from a terminal)."
    );
}

#[derive(Clone, Default)]
struct Session {
    clipboard: Rc<RefCell<Option<(mp3::Format, Frames)>>>,
    documents: Rc<Cell<usize>>,
}

impl Session {
    fn opening_document(&self) -> OpenDocument {
        self.documents.set(self.documents.get() + 1);
        OpenDocument(self.documents.clone())
    }
}

struct OpenDocument(Rc<Cell<usize>>);

impl Drop for OpenDocument {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}
