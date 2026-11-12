//! Read-only ID3 tag inventory.
//!
//! External MP3s often carry ID3v2 or ID3v1 tags. The archive path never
//! copies or modifies them: saving writes audio only, and the analyze report
//! says so. This module reads just enough of a tag to describe what is
//! present — text frames, artwork, ReplayGain values — so the report can say
//! exactly what a save would leave behind. Anything unusual is reported as
//! unrecognized rather than guessed at, and nothing here can fail the audio.

/// What could be read from one tag.
pub struct Inventory {
    /// Recognized frames as (label, value) pairs.
    pub entries: Vec<(String, String)>,
    /// Frames present but not recognized or not decodable.
    pub other: usize,
    /// Whether parsing reached the end of the tag cleanly.
    pub complete: bool,
}

/// The longest text kept in a summary before it is shortened.
const SUMMARY_LIMIT: usize = 48;

impl Inventory {
    /// One-line summary for report events, without the tag name.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = self
            .entries
            .iter()
            .map(|(label, value)| format!("{label}={value}"))
            .collect();
        if self.other > 0 {
            parts.push(format!("{} other frame(s)", self.other));
        }
        if !self.complete {
            parts.push("the rest of the tag could not be read".into());
        }
        if parts.is_empty() {
            return "no frames could be read".into();
        }
        parts.join(", ")
    }
}

fn summarize(text: &str) -> String {
    let mut text = text.replace('\0', " ").trim().to_string();
    if text.chars().count() > SUMMARY_LIMIT {
        let cut = text
            .char_indices()
            .nth(SUMMARY_LIMIT)
            .map(|(at, _)| at)
            .unwrap_or(text.len());
        text.truncate(cut);
        text.push_str("...");
    }
    text
}

/// Decode a text frame: one encoding byte, then the text.
fn text_content(body: &[u8]) -> String {
    let Some((&encoding, text)) = body.split_first() else {
        return String::new();
    };
    decode_text(encoding, text)
}

/// Decode text in the given ID3 encoding: 0 Latin-1, 1 UTF-16 with BOM,
/// 2 UTF-16 big-endian, 3 UTF-8.
fn decode_text(encoding: u8, text: &[u8]) -> String {
    match encoding {
        0 => text.iter().map(|&byte| byte as char).collect(),
        1 => utf16_text(text, false),
        2 => utf16_text(text, true),
        3 => String::from_utf8_lossy(text).into_owned(),
        _ => String::new(),
    }
}

/// UTF-16 text with an optional byte-order mark; `be` decides without one.
fn utf16_text(bytes: &[u8], be: bool) -> String {
    let (bytes, be) = match bytes {
        [0xFF, 0xFE, rest @ ..] => (rest, false),
        [0xFE, 0xFF, rest @ ..] => (rest, true),
        _ => (bytes, be),
    };
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| {
            if be {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// Undo ID3 unsynchronization: `FF 00` becomes `FF`.
fn unsynchronise(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut escape = false;
    for &byte in body {
        if escape && byte == 0 {
            escape = false;
            continue;
        }
        escape = byte == 0xFF;
        out.push(byte);
    }
    out
}

fn syncsafe(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .fold(0usize, |size, byte| (size << 7) | usize::from(*byte))
}

/// The ID3v2 frame header at the start of `body`, or `None` at padding or a
/// truncated tag. v2.2 uses 3-byte ids with 3-byte sizes and no flags.
struct FrameHeader {
    id: String,
    size: usize,
    /// Encrypted, compressed, or otherwise not plain content.
    opaque: bool,
    /// v2.4 only: the body carries its own unsynchronization.
    unsync: bool,
    /// v2.4 only: the body begins with a four-byte data length.
    data_length: bool,
    /// Byte length of the header itself.
    header_len: usize,
}

fn frame_header(body: &[u8], major: u8) -> Option<FrameHeader> {
    let id_len = if major == 2 { 3 } else { 4 };
    let id_bytes = body.get(..id_len)?;
    if id_bytes.iter().all(|&byte| byte == 0) {
        // Padding: the end of the frames.
        return None;
    }
    let id = String::from_utf8_lossy(id_bytes)
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();
    let size_start = id_len;
    let size_len = if major == 2 { 3 } else { 4 };
    let size_bytes = body.get(size_start..size_start + size_len)?;
    let size = if major >= 4 && size_len == 4 {
        syncsafe(size_bytes)
    } else {
        size_bytes
            .iter()
            .fold(0usize, |size, byte| (size << 8) | usize::from(*byte))
    };
    let header_len = size_start + size_len + usize::from(major != 2) * 2;
    let flags = if major != 2 {
        body.get(size_start + size_len + 1).copied().unwrap_or(0)
    } else {
        0
    };
    let unsync = major == 4 && flags & 0b10 != 0;
    let data_length = major == 4 && flags & 0b1 != 0;
    let opaque =
        (major == 3 && flags & 0b1110_0000 != 0) || (major == 4 && flags & 0b0100_1100 != 0);
    Some(FrameHeader {
        id,
        size,
        opaque,
        unsync,
        data_length,
        header_len,
    })
}

/// Labels for the frame ids worth naming; everything else is counted.
fn frame_label(id: &str) -> Option<&'static str> {
    match id {
        "TIT2" | "TT2" => Some("Title"),
        "TPE1" | "TP1" => Some("Artist"),
        "TPE2" | "TP2" => Some("Album artist"),
        "TALB" | "TAL" => Some("Album"),
        "TRCK" | "TRK" => Some("Track"),
        "TCON" | "TCO" => Some("Genre"),
        "TDRC" | "TYER" => Some("Year"),
        "USLT" | "ULT" => Some("Lyrics"),
        "COMM" | "COM" => Some("Comment"),
        _ => None,
    }
}

fn is_artwork(id: &str) -> bool {
    matches!(id, "APIC" | "PIC")
}

fn is_replaygain(description: &str) -> bool {
    description.to_ascii_uppercase().starts_with("REPLAYGAIN")
}

/// Read a whole ID3v2 tag: header, body, frames. `None` when `tag` does not
/// start with a usable ID3v2 header. The caller supplies the complete tag.
pub fn scan_v2(tag: &[u8]) -> Option<Inventory> {
    let header = tag.get(..10)?;
    if !header.starts_with(b"ID3") {
        return None;
    }
    let major = header[3];
    if !(2..=4).contains(&major) {
        return None;
    }
    let flags = header[5];
    let mut body = tag[10..].to_vec();
    if flags & 0x80 != 0 {
        body = unsynchronise(&body);
    }
    let mut at = 0usize;
    let mut complete = true;
    if flags & 0x40 != 0 && major >= 3 {
        // The extended header's size field covers the bytes after it.
        let Some(size_bytes) = body.get(at..at + 4) else {
            return Some(Inventory {
                entries: Vec::new(),
                other: 0,
                complete: false,
            });
        };
        let size = if major == 4 {
            syncsafe(size_bytes)
        } else {
            size_bytes
                .iter()
                .fold(0usize, |size, byte| (size << 8) | usize::from(*byte))
        };
        if 4 + size > body.len() - at {
            return Some(Inventory {
                entries: Vec::new(),
                other: 0,
                complete: false,
            });
        }
        at += 4 + size;
    }

    let mut entries = Vec::new();
    let mut other = 0usize;
    while at < body.len() {
        let Some(header) = frame_header(&body[at..], major) else {
            break;
        };
        let content_start = at + header.header_len;
        let Some(content_end) = content_start.checked_add(header.size) else {
            // An impossible size: not trustworthy, but the tag may still hold
            // readable frames after it, so resync one byte along.
            complete = false;
            at += 1;
            continue;
        };
        if content_end > body.len() {
            complete = false;
            at += 1;
            continue;
        }
        let mut content = body[content_start..content_end].to_vec();
        if header.unsync {
            content = unsynchronise(&content);
        }
        let content = if header.data_length && content.len() >= 4 {
            content[4..].to_vec()
        } else {
            content
        };
        at = content_end;
        if header.opaque {
            other += 1;
            continue;
        }
        if is_artwork(&header.id) {
            entries.push(("Artwork".into(), "embedded image".into()));
            continue;
        }
        if header.id == "TXXX" || header.id == "TXX" {
            // Encoding byte, description, then the value: ReplayGain values
            // live here.
            let Some((&encoding, rest)) = content.split_first() else {
                other += 1;
                continue;
            };
            let Some(split) = rest.iter().position(|&byte| byte == 0) else {
                other += 1;
                continue;
            };
            let description = decode_text(encoding, &rest[..split]);
            let value = decode_text(encoding, &rest[split + 1..]);
            if is_replaygain(&description) {
                // REPLAYGAIN_TRACK_GAIN reads as "ReplayGain track gain".
                let trimmed = description.trim();
                let rest = trimmed[10.min(trimmed.len())..]
                    .replace('_', " ")
                    .trim()
                    .to_lowercase();
                entries.push((format!("ReplayGain {rest}"), summarize(&value)));
            } else {
                other += 1;
            }
            continue;
        }
        if header.id == "RVA2" {
            entries.push((
                "ReplayGain adjustment (RVA2)".into(),
                summarize(&text_content(&content)),
            ));
            continue;
        }
        if let Some(label) = frame_label(&header.id) {
            entries.push((label.to_string(), summarize(&text_content(&content))));
            continue;
        }
        other += 1;
    }

    Some(Inventory {
        entries,
        other,
        complete,
    })
}

/// Read an ID3v1 tag: a fixed 128-byte trailer starting with `TAG`. Text is
/// Latin-1 by definition.
pub fn scan_v1(tag: &[u8]) -> Option<Inventory> {
    if tag.len() != 128 || !tag.starts_with(b"TAG") {
        return None;
    }
    let field = |range: std::ops::Range<usize>| -> String {
        summarize(
            &tag[range]
                .iter()
                .map(|&byte| byte as char)
                .collect::<String>(),
        )
    };
    let mut entries = Vec::new();
    for (label, range) in [
        ("Title", 3..33),
        ("Artist", 33..63),
        ("Album", 63..93),
        ("Year", 93..97),
    ] {
        let value = field(range);
        if !value.is_empty() {
            entries.push((label.to_string(), value));
        }
    }
    Some(Inventory {
        entries,
        other: 0,
        complete: true,
    })
}

/// Build a synthetic ID3v2.3 tag from raw frame bodies.
#[cfg(test)]
fn v2_tag(frames: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, content) in frames {
        body.extend_from_slice(id.as_bytes());
        body.extend_from_slice(&u32::try_from(content.len()).unwrap().to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(content);
    }
    let mut tag: Vec<u8> = b"ID3".to_vec();
    tag.push(3);
    tag.push(0);
    tag.push(0);
    tag.extend_from_slice(&syncsafe_u32(body.len() as u32));
    tag.extend_from_slice(&body);
    tag
}

#[cfg(test)]
fn syncsafe_u32(value: u32) -> [u8; 4] {
    [
        (value >> 21) as u8 & 0x7F,
        (value >> 14) as u8 & 0x7F,
        (value >> 7) as u8 & 0x7F,
        value as u8 & 0x7F,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latin1(text: &str) -> Vec<u8> {
        let mut out = vec![0u8];
        out.extend_from_slice(text.as_bytes());
        out
    }

    #[test]
    fn reads_common_frames_and_replaygain() {
        let mut txxx = vec![0u8];
        txxx.extend_from_slice(b"REPLAYGAIN_TRACK_GAIN");
        txxx.push(0);
        txxx.extend_from_slice(b"-7.89 dB");
        let tag = v2_tag(&[
            ("TIT2", &latin1("Warm")),
            ("TPE1", &latin1("Test Artist")),
            ("TXXX", &txxx),
        ]);
        let inventory = scan_v2(&tag).unwrap();
        assert!(inventory.complete);
        assert_eq!(inventory.entries[0], ("Title".into(), "Warm".into()));
        assert_eq!(
            inventory.entries[1],
            ("Artist".into(), "Test Artist".into())
        );
        assert_eq!(
            inventory.entries[2],
            ("ReplayGain track gain".into(), "-7.89 dB".into())
        );
    }

    #[test]
    fn detects_artwork_and_counts_unrecognized() {
        let tag = v2_tag(&[("APIC", &[0, 0, 1, 2, 3, 4, 5]), ("PRIV", &[1, 2, 3])]);
        let inventory = scan_v2(&tag).unwrap();
        assert_eq!(inventory.entries[0].0, "Artwork");
        assert_eq!(inventory.other, 1);
    }

    #[test]
    fn survives_a_frame_size_past_the_tag() {
        let mut body: Vec<u8> = b"TIT2".to_vec();
        body.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(b"TPE1");
        body.extend_from_slice(&8u32.to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        let mut late = latin1("Late");
        late.resize(8, 0);
        body.extend_from_slice(&late);
        let mut tag: Vec<u8> = b"ID3".to_vec();
        tag.push(3);
        tag.push(0);
        tag.push(0);
        tag.extend_from_slice(&syncsafe_u32(body.len() as u32));
        tag.extend_from_slice(&body);
        let inventory = scan_v2(&tag).unwrap();
        assert!(!inventory.complete);
        assert!(inventory.entries.iter().any(|(label, _)| label == "Artist"));
    }

    #[test]
    fn reads_id3v1_fields() {
        let mut tag = vec![0u8; 128];
        tag[..3].copy_from_slice(b"TAG");
        tag[3..7].copy_from_slice(b"Warm");
        let inventory = scan_v1(&tag).unwrap();
        assert_eq!(inventory.entries[0], ("Title".into(), "Warm".into()));
    }

    #[test]
    fn rejects_a_non_tag() {
        assert!(scan_v2(b"XID3").is_none());
        assert!(scan_v1(&[0u8; 128]).is_none());
    }

    #[test]
    fn summary_is_one_line() {
        let tag = v2_tag(&[("TIT2", &latin1("Warm"))]);
        let inventory = scan_v2(&tag).unwrap();
        let summary = inventory.summary();
        assert!(summary.starts_with("Title=Warm"), "{summary}");
        assert!(!summary.contains('\n'), "{summary}");
    }
}
