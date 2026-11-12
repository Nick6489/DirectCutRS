//! CUE and archive trim helpers.
use crate::{audio, mp3};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub fn cue_time(seconds: f64) -> String {
    let total = (seconds.max(0.0) * 75.0).round() as u64;
    let minutes = total / (75 * 60);
    let seconds = total / 75 % 60;
    let frames = total % 75;
    format!("{minutes:02}:{seconds:02}:{frames:02}")
}

fn cue_seconds(text: &str) -> Option<f64> {
    let mut parts = text.trim().split(':');
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    let frames: f64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(minutes * 60.0 + seconds + frames / 75.0)
}

fn cue_file_type(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".mp3") {
        "MP3"
    } else if lower.ends_with(".wav") {
        "WAVE"
    } else {
        "BINARY"
    }
}

fn cue_title(title: &str) -> String {
    title
        .replace('"', "'")
        .replace(['\r', '\n'], " ")
        .trim()
        .to_string()
}

#[cfg(test)]
pub fn write_cue(path: &Path, audio_name: &str, entries: &[(f64, String)]) -> io::Result<()> {
    write_cue_checked(path, audio_name, entries, true)
}

pub fn write_cue_checked(
    path: &Path,
    audio_name: &str,
    entries: &[(f64, String)],
    replace: bool,
) -> io::Result<()> {
    persist_cue(path, &cue_contents(audio_name, entries), replace, None)
}

pub fn write_editor_cue(
    path: &Path,
    audio_name: &str,
    entries: &[(f64, String)],
    expected: Option<&[u8]>,
) -> io::Result<Vec<u8>> {
    let contents = cue_contents(audio_name, entries);
    persist_cue(path, &contents, expected.is_some(), expected)?;
    Ok(contents)
}

fn cue_contents(audio_name: &str, entries: &[(f64, String)]) -> Vec<u8> {
    let mut text = format!(
        "FILE \"{}\" {}\n\n",
        audio_name.replace('"', "'"),
        cue_file_type(audio_name)
    );
    for (index, (seconds, title)) in entries.iter().enumerate() {
        text.push_str(&format!(
            "  TRACK {:02} AUDIO\n    TITLE \"{}\"\n    INDEX 01 {}\n\n",
            index + 1,
            cue_title(title),
            cue_time(*seconds)
        ));
    }
    text.into_bytes()
}

fn persist_cue(
    path: &Path,
    contents: &[u8],
    replace: bool,
    expected: Option<&[u8]>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    if let Some(expected) = expected {
        if fs::read(path)? != expected {
            return Err(io::Error::other(
                "The existing CUE changed and was left in place.",
            ));
        }
    }
    if replace {
        temp.persist(path).map_err(|error| error.error)?;
    } else {
        temp.persist_noclobber(path).map_err(|error| error.error)?;
    }
    Ok(())
}

/// Read the track titles and start times from a CUE sheet written here or by
/// the Stream Title Logger.
pub fn read_cue(path: &Path) -> io::Result<Vec<(f64, String)>> {
    let text = fs::read_to_string(path)?;
    let mut entries = Vec::new();
    let mut title: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("TITLE ") {
            title = Some(line[6..].trim().trim_matches('"').to_string());
        } else if upper.starts_with("INDEX 01 ")
            && let (Some(title), Some(seconds)) = (title.take(), cue_seconds(&line[9..]))
        {
            entries.push((seconds, title));
        }
    }
    Ok(entries)
}

fn csv_field(text: &str) -> String {
    if text.chars().any(|ch| matches!(ch, ',' | '"' | '\r' | '\n')) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            _ => field.push(ch),
        }
    }
    fields.push(field);
    fields
}

pub struct TrimResult {
    pub audio: PathBuf,
    pub cue: Option<PathBuf>,
    pub csv: Option<PathBuf>,
    /// Sidecar CSV failure does not invalidate successfully exported MP3/CUE.
    pub csv_warning: Option<String>,
    pub start: f64,
    pub end: f64,
    pub frames: usize,
}

/// Trim an archived MP3 at CUE track boundaries without re-encoding audio.
/// The source file is read only. A new `_trimmed` MP3, CUE, and (when possible)
/// CSV are written next to it; an existing file is never overwritten.
pub fn trim_mp3(
    audio_path: &Path,
    cue_path: &Path,
    start_index: usize,
    end_index: usize,
) -> Result<TrimResult, String> {
    let entries =
        read_cue(cue_path).map_err(|error| format!("Could not read the CUE sheet: {error}"))?;
    if entries.is_empty() {
        return Err("The CUE sheet contains no track entries.".into());
    }
    if start_index >= entries.len() || end_index >= entries.len() || end_index < start_index {
        return Err("Choose a start title and an end title in order.".into());
    }
    let bytes = fs::read(audio_path)
        .map_err(|error| format!("Could not read {}: {error}", audio_path.display()))?;
    let scan = mp3::scan(&bytes)
        .map_err(|error| format!("{} is not a recoverable MP3: {error}", audio_path.display()))?;
    if scan.frame_count == 0 {
        return Err("The archive contains no MP3 audio frames.".into());
    }
    let frame_seconds = scan.format.samples() as f64 / f64::from(scan.format.rate);
    let duration = scan.frame_count as f64 * frame_seconds;
    let start = entries[start_index].0.max(0.0).min(duration);
    let end = if end_index + 1 < entries.len() {
        entries[end_index + 1].0
    } else {
        duration
    }
    .min(duration);
    if end <= start {
        return Err("The chosen end point must be after the start point.".into());
    }
    let start_frame = ((start / frame_seconds).round() as usize).min(scan.frame_count - 1);
    let end_frame = ((end / frame_seconds).round() as usize)
        .max(start_frame + 1)
        .min(scan.frame_count);
    // Report and offset titles by the frame-aligned boundaries that were actually
    // written, not by the raw CUE times.
    let start = start_frame as f64 * frame_seconds;
    let end = end_frame as f64 * frame_seconds;
    let frames = scan.frames[start_frame..end_frame]
        .iter()
        .map(|frame| std::sync::Arc::new(crate::codec::Frame::Mp3(std::sync::Arc::clone(frame))))
        .collect();
    let output = unique_output(audio_path, "_trimmed", "mp3")?;
    let audio_result = audio::export_no_replace(
        &output,
        crate::codec::Info::Mp3 {
            format: scan.format,
        },
        frames,
    )
    .map_err(|error| format!("Could not write the trimmed audio: {error}"))?;
    if audio_result == audio::NoReplace::Existed {
        return Err(format!(
            "The trim destination {} is already in use. Run the trim again to choose another name.",
            output.display()
        ));
    }

    let mut trimmed: Vec<(f64, String)> = entries
        .iter()
        .enumerate()
        .filter(|(index, (seconds, _))| *index >= start_index && *seconds < end)
        .map(|(index, (seconds, title))| {
            let offset = if index == start_index {
                0.0
            } else {
                (seconds - start).max(0.0)
            };
            (offset, title.clone())
        })
        .collect();
    if trimmed.is_empty() {
        trimmed.push((0.0, entries[start_index].1.clone()));
    } else if trimmed[0].0 > 0.01 {
        trimmed.insert(0, (0.0, entries[start_index].1.clone()));
    } else {
        trimmed[0].0 = 0.0;
    }
    let out_name = output
        .file_name()
        .unwrap_or(output.as_os_str())
        .to_string_lossy()
        .into_owned();
    let cue = output.with_extension("cue");
    write_cue_checked(&cue, &out_name, &trimmed, false).map_err(|error| {
        format!(
            "The trimmed audio was saved to {}, but its CUE sheet could not be written to {}: {error}. The original archive was not changed.",
            output.display(),
            cue.display()
        )
    })?;

    let csv_result = rewrite_trimmed_csv(
        &audio_path.with_file_name("titles.csv"),
        &output.with_extension("csv"),
        segment_number(audio_path),
        start,
        end,
    );
    let (csv, csv_warning) = match csv_result {
        Ok(csv) => (csv, None),
        Err(error) => (
            None,
            Some(format!(
                "The optional titles CSV could not be written: {error}"
            )),
        ),
    };

    Ok(TrimResult {
        audio: output,
        cue: Some(cue),
        csv,
        csv_warning,
        start,
        end,
        frames: end_frame - start_frame,
    })
}

fn unique_output(source: &Path, suffix: &str, extension: &str) -> Result<PathBuf, String> {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let stem = source
        .file_stem()
        .unwrap_or(source.as_os_str())
        .to_string_lossy();
    for count in 1..=10_000 {
        let name = if count == 1 {
            format!("{stem}{suffix}.{extension}")
        } else {
            format!("{stem}{suffix}_{count}.{extension}")
        };
        let candidate = parent.join(name);
        if !candidate.exists()
            && !candidate.with_extension("cue").exists()
            && !candidate.with_extension("csv").exists()
        {
            return Ok(candidate);
        }
    }
    Err("Could not choose a free output name.".into())
}

fn segment_number(path: &Path) -> Option<u32> {
    let stem = path.file_stem()?.to_string_lossy();
    let (_, rest) = stem.rsplit_once("_part")?;
    rest.parse().ok()
}

fn rewrite_trimmed_csv(
    source: &Path,
    output: &Path,
    segment: Option<u32>,
    start: f64,
    end: f64,
) -> io::Result<Option<PathBuf>> {
    let Some(segment) = segment else {
        return Ok(None);
    };
    let Ok(text) = fs::read_to_string(source) else {
        return Ok(None);
    };
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    let Some(header) = lines.next() else {
        return Ok(None);
    };
    let fields: Vec<_> = parse_csv_line(header);
    let column = |name: &str| fields.iter().position(|field| field == name);
    let (Some(timestamp), Some(segment_col), Some(elapsed_col), Some(title), Some(event)) = (
        column("timestamp_local"),
        column("segment"),
        column("elapsed_seconds"),
        column("title"),
        column("event"),
    ) else {
        return Ok(None);
    };
    let mut kept = Vec::new();
    for line in lines {
        let row = parse_csv_line(line);
        let get = |index: usize| row.get(index).map(String::as_str).unwrap_or("");
        if get(event) != "title" || get(segment_col).parse::<u32>().ok() != Some(segment) {
            continue;
        }
        let Ok(elapsed) = get(elapsed_col).parse::<f64>() else {
            continue;
        };
        if elapsed < start || elapsed >= end {
            continue;
        }
        kept.push((
            get(timestamp).to_string(),
            format!("{:.3}", (elapsed - start).max(0.0)),
            get(title).to_string(),
        ));
    }
    let mut text = String::from("\u{feff}");
    text.push_str("timestamp_local,segment,elapsed_seconds,title,event\r\n");
    text.push_str(",1,0.000,,trimmed_archive_start\r\n");
    for (timestamp, elapsed, title) in kept {
        text.push_str(&format!(
            "{},1,{},{},title\r\n",
            csv_field(&timestamp),
            elapsed,
            csv_field(&title)
        ));
    }
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(text.as_bytes())?;
    file.persist_noclobber(output)
        .map_err(|error| error.error)?;
    Ok(Some(output.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_cue_does_not_replace_changed_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit.cue");
        let entries = [(0.0, "Title".into())];
        let expected = write_editor_cue(&path, "edit.mp3", &entries, None).unwrap();
        fs::write(&path, b"externally replaced").unwrap();
        assert!(write_editor_cue(&path, "edit.mp3", &entries, Some(&expected)).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"externally replaced");
    }

    #[test]
    fn cue_without_replace_preserves_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing.cue");
        fs::write(&path, "original").unwrap();
        assert!(write_cue_checked(&path, "audio.mp3", &[(0.0, "Title".into())], false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    }

    #[test]
    fn cue_times_and_titles_round_trip() {
        assert_eq!(cue_time(0.0), "00:00:00");
        assert_eq!(cue_time(61.5), "01:01:38");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("part.cue");
        write_cue(
            &path,
            "part.mp3",
            &[(0.0, "First \"song\"".into()), (61.5, "Second".into())],
        )
        .unwrap();
        let entries = read_cue(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], (0.0, "First 'song'".into()));
        // CUE stores 75 frames per second, so compare within one frame.
        assert!((entries[1].0 - 61.5).abs() < 1.0 / 75.0);
    }

    #[test]
    #[ignore = "requires generated MP3 fixtures in DIRECTCUT_TEST_FIXTURES"]
    fn trims_an_archived_mp3_without_touching_the_source() {
        let Ok(fixtures) = std::env::var("DIRECTCUT_TEST_FIXTURES") else {
            return;
        };
        let source = Path::new(&fixtures).join("cbr.mp3");
        if !source.exists() {
            return;
        }
        let work = tempfile::tempdir().unwrap();
        let audio = work.path().join("Station_part001.mp3");
        fs::copy(&source, &audio).unwrap();
        let before = fs::read(&audio).unwrap();
        let cue = work.path().join("Station_part001.cue");
        write_cue(
            &cue,
            "Station_part001.mp3",
            &[(0.0, "First".into()), (1.0, "Second".into())],
        )
        .unwrap();
        fs::write(
            work.path().join("titles.csv"),
            "\u{feff}timestamp_local,segment,elapsed_seconds,title,event\r\n\
             2026-01-01T00:00:00,1,0.000,First,title\r\n\
             2026-01-01T00:00:01,1,1.000,Second,title\r\n",
        )
        .unwrap();

        let result = trim_mp3(&audio, &cue, 0, 0).unwrap();

        assert!(result.audio.exists());
        assert!(result.frames > 0);
        assert_eq!(fs::read(&audio).unwrap(), before);
        assert!(result.cue.as_ref().is_some_and(|path| path.exists()));
        assert!(result.csv.as_ref().is_some_and(|path| path.exists()));
        let scan = mp3::scan(&fs::read(&result.audio).unwrap()).unwrap();
        assert!(scan.frame_count > 0);
        let trimmed_cue = fs::read_to_string(result.cue.unwrap()).unwrap();
        assert!(trimmed_cue.contains("First"));
        assert!(!trimmed_cue.contains("Second"));
    }
}
