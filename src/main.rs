#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod editor;
mod mp3;
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
            "DirectCut RS 1.0\nBy Nick Giannak III\nMade exclusively with artificial intelligence.\nCopyright © 2026 Nick Giannak III\nMIT License");
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
    start.set_size(360, 108);
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
