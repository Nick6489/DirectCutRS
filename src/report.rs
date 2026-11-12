//! Plain-text archive reports.
//!
//! The report is deliberately conservative: it separates what was observed from
//! what is only estimated, gives file byte ranges (never invented time ranges),
//! and states which checks did *not* run.

use std::{fs, path::Path, time::Duration};

use sha2::{Digest, Sha256};

use crate::codec;
use crate::mp3::{self, EventKind, Result, Scan};

/// Suggested name for a report sidecar.
pub fn suggested_name(source: &Path, suffix: &str) -> String {
    format!(
        "{} - {suffix}.txt",
        source
            .file_stem()
            .unwrap_or(source.as_os_str())
            .to_string_lossy()
    )
}

pub fn file_label(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            use std::fmt::Write as _;
            let _ = write!(text, "{byte:02x}");
            text
        })
}

/// Read and analyze `path` without modifying it. The report includes the input
/// hash so a later repair can be tied to this exact file.
pub fn analyze_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    let scan = mp3::inspect(&bytes)?;
    Ok(build(&scan, &file_label(path), Some(&sha256_hex(&bytes))))
}

fn time(duration: Duration) -> String {
    let millis = duration.as_millis();
    format!(
        "{}:{:02}:{:02}.{:03}",
        millis / 3_600_000,
        millis / 60_000 % 60,
        millis / 1_000 % 60,
        millis % 1_000
    )
}

fn channel_text(channels: u8) -> &'static str {
    if channels == 1 { "mono" } else { "stereo" }
}

fn mpeg_name(version: u8) -> &'static str {
    match version {
        3 => "MPEG-1",
        2 => "MPEG-2",
        _ => "MPEG-2.5",
    }
}

fn classification(scan: &Scan) -> &'static str {
    if scan
        .events
        .iter()
        .any(|event| event.kind == EventKind::FormatChange)
    {
        "Partial stream. An incompatible format change stopped the import."
    } else if scan.has_gaps() {
        "Recovered with gaps. Audio on each side of a gap is joined; nothing was inserted."
    } else if scan
        .events
        .iter()
        .any(|event| event.kind == EventKind::Metadata && event.detail.contains("invalid"))
    {
        "Audio frame sequence is intact. A metadata block needs attention."
    } else {
        "Intact MPEG frame sequence. No audio gaps were found."
    }
}

/// Build a complete plain-text report. `label` should be a file name, not a full
/// path, unless the user explicitly wants the path recorded.
pub fn build(scan: &Scan, label: &str, hash: Option<&str>) -> String {
    use std::fmt::Write as _;

    let mut text = String::new();
    text.push_str("DirectCut RS archive analysis\r\n");
    text.push_str("Report format 1\r\n\r\n");
    let _ = writeln!(text, "File name: {label}");
    let _ = writeln!(text, "Input size: {} bytes", scan.input_len);
    if let Some(hash) = hash {
        let _ = writeln!(text, "SHA-256: {hash}");
    }
    let _ = writeln!(
        text,
        "Audio starts at byte {} after any leading tag data.",
        scan.audio_offset
    );
    text.push_str("\r\n");

    let _ = writeln!(
        text,
        "Stream: {} Layer III, {} Hz, {} ({})",
        mpeg_name(scan.format.version()),
        scan.format.rate,
        channel_text(scan.format.channels),
        scan.format.channels
    );
    let _ = writeln!(
        text,
        "Retained audio: {} frames, {}",
        scan.frame_count,
        time(scan.duration())
    );
    let _ = writeln!(
        text,
        "Retained frame bytes: {} (headers and side information included)",
        scan.retained_bytes
    );
    let _ = writeln!(
        text,
        "Skipped: {} bytes; {} frame(s) needed unavailable reservoir data.",
        scan.recovery.skipped_bytes, scan.recovery.skipped_frames
    );
    let _ = writeln!(text, "Status: {}", classification(scan));
    text.push_str(
        "Validation: structural parsing only. No decoder or encoder ran during this analysis.\r\n",
    );
    let _ = writeln!(text, "{}", codec::MP3.summary());
    text.push_str("\r\n");

    if scan.events.is_empty() {
        text.push_str("Events: none.\r\n");
    } else {
        text.push_str("Events (byte offsets in the original file):\r\n");
        for (index, event) in scan.events.iter().enumerate() {
            let _ = writeln!(text, "  {}. {}", index + 1, event.describe());
        }
    }
    if scan.suppressed_events > 0 {
        let _ = writeln!(
            text,
            "  ... {} further event(s) were merged or omitted to keep this report bounded.",
            scan.suppressed_events
        );
    }
    text.push_str("\r\n");

    let metadata: Vec<_> = scan
        .events
        .iter()
        .filter(|event| event.kind == EventKind::Metadata)
        .collect();
    if metadata.is_empty() {
        text.push_str(
            "Metadata: no ID3v1, ID3v2, or APEv2 tag was detected at the file boundaries.\r\n",
        );
    } else {
        text.push_str("Metadata (this build does not copy tags on export):\r\n");
        for event in metadata {
            let _ = writeln!(text, "  - {}", event.describe());
        }
    }
    text.push_str("\r\n");

    text.push_str("Honest limits:\r\n");
    text.push_str("  - Byte ranges are exact file positions, not time ranges. Skipped audio has no known duration.\r\n");
    text.push_str("  - A gap joins retained frames directly. Nothing was replaced with silence or transcoded audio.\r\n");
    text.push_str("  - Recovered duration counts retained frames only; encoder delay and padding are not known here.\r\n");
    text.push_str("  - A successful parse does not prove that every decoder will interpret the stream identically.\r\n");
    text.push_str("  - This analysis did not modify the input file.\r\n");
    text
}

/// Short text for the editor's timeline status (F6). Returns `None` for a clean
/// import so healthy files stay quiet.
pub fn summary(timeline: &codec::Timeline) -> Option<String> {
    if !timeline.has_gaps() {
        return None;
    }
    let problems = timeline
        .events
        .iter()
        .filter(|event| event.kind.is_problem())
        .count();
    let mut text = format!(
        "Import recovery: {} skipped byte(s), {} frame(s) skipped, {} event(s).",
        timeline.recovery.skipped_bytes,
        timeline.recovery.skipped_frames,
        problems + timeline.suppressed_events,
    );
    for (shown, event) in timeline
        .events
        .iter()
        .filter(|event| event.kind.is_problem())
        .enumerate()
    {
        if shown == 3 {
            if timeline.info.kind() == codec::Kind::Mp3 {
                text.push_str("\r\nMore events are listed by Analyze Source MP3.");
            } else {
                text.push_str("\r\nMore events were omitted from this summary.");
            }
            break;
        }
        text.push_str("\r\n- ");
        text.push_str(&event.describe());
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_a_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn suggested_report_name_keeps_the_source_stem() {
        assert_eq!(
            suggested_name(Path::new("C:\\archive\\show.mp3"), "analysis"),
            "show - analysis.txt"
        );
    }
}
