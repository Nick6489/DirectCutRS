//! Direct screen-reader speech through Prism.

#[cfg(windows)]
use prismer::{BackendId, Features, Prism};

#[cfg(windows)]
thread_local! {
    static PRISM: Option<Prism> = Prism::new().ok();
}

pub fn announce(text: &str) {
    if text.is_empty() {
        return;
    }
    #[cfg(windows)]
    PRISM.with(|prism| {
        let Some(prism) = prism else { return };
        // Prefer direct screen-reader backends. Keep the app quiet when no
        // reader is running, and avoid the UIA notification path.
        let mut readers = prism.backend_ids();
        readers.sort_by_key(|&id| std::cmp::Reverse(prism.backend_priority(id)));
        for id in readers {
            if id == BackendId::UIA {
                continue;
            }
            let Ok(backend) = prism.create(id) else {
                continue;
            };
            if backend
                .features()
                .contains(Features::IS_SCREEN_READER | Features::SPEAK)
                && backend.initialize().is_ok()
                && backend.speak(text, false).is_ok()
            {
                break;
            }
        }
    });
}
