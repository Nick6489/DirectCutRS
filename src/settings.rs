//! Portable application settings.
//!
//! Stored as `settings.toml` beside the executable: human-readable and easy
//! to edit while no window is open. This file covers the app-wide choices — the
//! playback device, the last scrub zoom, the default playback speed, the
//! loop segment scrub toggle, and whether playback continues after a scrub —
//! so an editor window opens exactly where the last session left them.

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

/// What could be read from one settings file.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AppSettings {
    pub playback: Playback,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Playback {
    /// The chosen playback device; absent is the system default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The scrub step in frames, from the zoom keys.
    #[serde(default = "default_scrub_step")]
    pub scrub_step: usize,
    /// The playback speed carried between sessions.
    #[serde(default = "default_speed")]
    pub speed: f32,
    /// Whether loop segment scrub (/) was left on.
    #[serde(default)]
    pub scrub_loop: bool,
    /// Whether playback continues from the playhead when a scrub snippet
    /// ends.
    #[serde(default = "default_resume_after_scrub")]
    pub resume_after_scrub: bool,
    #[serde(default = "default_resume_after_scrub")]
    pub confirm_long_paste: bool,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            device: None,
            scrub_step: default_scrub_step(),
            speed: default_speed(),
            scrub_loop: false,
            resume_after_scrub: default_resume_after_scrub(),
            confirm_long_paste: true,
        }
    }
}

fn default_scrub_step() -> usize {
    1
}

fn default_speed() -> f32 {
    1.0
}

fn default_resume_after_scrub() -> bool {
    true
}

/// Read `settings.toml`. A missing file or missing fields fall back to the
/// defaults; a file that cannot be parsed is moved aside as
/// `settings.toml.broken` rather than overwritten silently, and the defaults
/// apply.
pub fn load(path: &Path) -> AppSettings {
    let Ok(text) = fs::read_to_string(path) else {
        return AppSettings {
            playback: Playback::default(),
        };
    };
    match toml::from_str::<AppSettings>(&text) {
        Ok(settings) => settings,
        Err(_) => {
            let _ = fs::rename(path, path.with_extension("toml.broken"));
            AppSettings {
                playback: Playback::default(),
            }
        }
    }
}

/// Write `settings.toml`: written beside the real file first so a crash
/// cannot truncate the settings.
pub fn store(path: &Path, settings: &AppSettings) -> std::io::Result<()> {
    let body = toml::to_string_pretty(settings)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, body)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}
