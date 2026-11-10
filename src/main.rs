#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod editor;
mod mp3;

use std::{cell::RefCell, path::PathBuf, rc::Rc};

use mp3::Frames;
use rungui::*;

fn main() {
    let app = App::new("DirectCut RS").expect("Could not initialize DirectCut RS");
    let clipboard = Rc::new(RefCell::new(None));
    let start = Window::new("DirectCut RS");
    let layout = VBox::new(start);
    layout.set_padding(16);
    layout.set_spacing(12);
    Label::new(layout, "Open an MP3 to start editing.");
    let open = Button::new(layout, "&Open MP3...");
    let file = Menu::new(MenuBar::new(start), "&File");
    let menu_open = MenuItem::new(file, "&Open MP3...");
    menu_open.set_accel("Ctrl+O");
    open.on_click({
        let clipboard = clipboard.clone();
        move || editor::choose_files(start, &clipboard)
    });
    menu_open.on_click({
        let clipboard = clipboard.clone();
        move || editor::choose_files(start, &clipboard)
    });
    start.set_size(400, 118);
    start.show();
    open.focus();

    for path in std::env::args_os().skip(1).map(PathBuf::from) {
        editor::open_file(start, path, &clipboard);
    }
    app.run();
}

type Clipboard = Rc<RefCell<Option<(mp3::Format, Frames)>>>;
