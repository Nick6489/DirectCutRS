//! MPEG Layer II frame walking.
//!
//! Layer II frames are self-contained: each frame header carries its own
//! length, and no frame needs data from any other frame, so a lossless splice
//! copies frames byte-exactly and no repacking is required. The walk is
//! stricter than the MP3 importer on purpose — there is no resynchronisation
//! after a damaged frame, because unlike Layer III there is no reservoir
//! state to lose, and refusing to guess keeps the timeline honest.

use std::sync::Arc;

use super::{Events, Frame, Info, Timeline};
use crate::mp3::{EventKind, Result};

/// MPEG-1 Layer II bitrate indexes in kbit/s; index 0 is free format and 15 is
/// forbidden.
const MPEG1: [usize; 15] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
];
/// MPEG-2 and MPEG-2.5 Layer II bitrates, which share the Layer III LSF table.
const LSF: [usize; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Header {
    /// MPEG version bits as stored in the header: 3 = MPEG-1, 2 = MPEG-2,
    /// 0 = MPEG-2.5.
    version: u8,
    rate: u32,
    channels: u8,
    len: usize,
}

impl Header {
    /// The same frame-length rule the MP3 importer uses, with Layer II's
    /// bitrate table: 144000 * bitrate / rate, plus one byte when padded.
    /// Layer II keeps 1152 samples per frame in every version.
    pub(super) fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 4 {
            return None;
        }
        let h = [bytes[0], bytes[1], bytes[2], bytes[3]];
        let version = (h[1] >> 3) & 3;
        if h[0] != 0xFF
            || h[1] & 0xE0 != 0xE0
            || version == 1
            || (h[1] >> 1) & 3 != 2
            || matches!(h[2] >> 4, 0 | 15)
        {
            return None;
        }
        let base_rate = [44_100u32, 48_000, 32_000, 0][((h[2] >> 2) & 3) as usize];
        if base_rate == 0 {
            return None;
        }
        let rate = base_rate
            >> match version {
                3 => 0,
                2 => 1,
                _ => 2,
            };
        let bitrate = if version == 3 {
            MPEG1[(h[2] >> 4) as usize]
        } else {
            LSF[(h[2] >> 4) as usize]
        };
        let padding = (h[2] >> 1) & 1 == 1;
        Some(Self {
            version,
            rate,
            channels: if h[3] >> 6 == 3 { 1 } else { 2 },
            len: 144_000 * bitrate / rate as usize + usize::from(padding),
        })
    }

    fn info(self) -> Info {
        Info::Mp2 {
            rate: self.rate,
            channels: self.channels,
        }
    }

    /// The format description and total byte length for format detection.
    pub(super) fn parts(self) -> (Info, usize) {
        (self.info(), self.len)
    }

    fn matches(self, other: Self) -> bool {
        self.version == other.version && self.rate == other.rate && self.channels == other.channels
    }
}

/// Walk the whole file. The first frame is accepted only when the frame after
/// it is a valid Layer II frame of the same format, so a stray 0xFF inside
/// garbage is not mistaken for audio. Every byte before that is leading
/// garbage; the walk then follows headers one frame at a time and stops at the
/// first damaged or truncated frame.
pub fn scan(bytes: &[u8]) -> Result<Timeline> {
    let mut events = Events::new();
    let Some((offset, first)) = verified_start(bytes) else {
        return Err("No MPEG Layer II frame sequence was found in the file.".into());
    };
    if offset > 0 {
        events.push(
            EventKind::LeadingGarbage,
            0,
            offset,
            0,
            "bytes before the first verified Layer II frame",
        );
    }
    let info = first.info();
    let mut frames = Vec::new();
    let mut position = offset;
    while position < bytes.len() {
        let Some(header) = Header::parse(&bytes[position..]) else {
            let message = if bytes.len() - position < first.len {
                "a damaged header stopped the import near the end of the file"
            } else {
                "a damaged header stopped the import"
            };
            events.push(EventKind::InvalidHeader, position, bytes.len(), 0, message);
            break;
        };
        if !first.matches(header) {
            events.push(
                EventKind::FormatChange,
                position,
                bytes.len(),
                0,
                "the MPEG version, sample rate, or channel count changed",
            );
            break;
        }
        if position + header.len > bytes.len() {
            events.push(
                EventKind::TruncatedFrame,
                position,
                bytes.len(),
                1,
                "an incomplete frame was skipped at the end of the file",
            );
            break;
        }
        frames.push(Arc::new(Frame::Data(
            bytes[position..position + header.len].into(),
        )));
        position += header.len;
    }
    let (events, recovery, suppressed) = events.finish(0);
    Ok(Timeline {
        info,
        frames,
        recovery,
        events,
        suppressed_events: suppressed,
    })
}

/// Find the first byte offset that starts a Layer II frame followed by a
/// compatible Layer II frame.
fn verified_start(bytes: &[u8]) -> Option<(usize, Header)> {
    let mut position = 0;
    while position + 4 <= bytes.len() {
        if let Some(header) = Header::parse(&bytes[position..]) {
            let next = position + header.len;
            if next + 4 <= bytes.len()
                && Header::parse(&bytes[next..]).is_some_and(|follower| header.matches(follower))
            {
                return Some((position, header));
            }
        }
        position += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Header, scan};
    use crate::mp3::EventKind;

    /// Build one complete Layer II frame: a real header followed by zero
    /// payload up to the header's own frame length, so `Header::parse` agrees
    /// with what is on the wire.
    fn frame(version: u8, bitrate_index: usize, rate_index: u8, padding: bool) -> Vec<u8> {
        let mut bytes = vec![
            0xFFu8,
            0b1111_0001 | (version << 3) | (2 << 1),
            ((bitrate_index as u8) << 4) | (rate_index << 2) | (u8::from(padding) << 1),
            0x00,
        ];
        let len = Header::parse(&bytes)
            .expect("the test header must be valid")
            .len;
        bytes.resize(len, 0);
        bytes
    }

    #[test]
    fn parses_a_layer_ii_sequence() {
        let bytes = [
            frame(3, 9, 0, false),
            frame(3, 9, 0, false),
            frame(3, 9, 0, false),
        ]
        .concat();
        let timeline = scan(&bytes).unwrap();
        assert_eq!(timeline.frames.len(), 3);
        assert_eq!(timeline.info.rate(), 44_100);
        assert_eq!(timeline.info.channels(), 2);
        assert_eq!(timeline.info.samples(), 1152);
        assert!(timeline.events.is_empty());
        assert!(!timeline.has_gaps());
    }

    #[test]
    fn reports_leading_garbage_and_a_damaged_tail() {
        let mut bytes = vec![0x00; 5];
        bytes.extend([frame(3, 9, 0, false), frame(3, 9, 0, false)].concat());
        bytes.push(0xFF);
        let timeline = scan(&bytes).unwrap();
        assert_eq!(timeline.frames.len(), 2);
        assert!(
            timeline
                .events
                .iter()
                .any(|event| event.kind == EventKind::LeadingGarbage
                    && event.start == 0
                    && event.end == 5)
        );
        assert!(
            timeline
                .events
                .iter()
                .any(|event| event.kind == EventKind::InvalidHeader)
        );
        assert!(timeline.has_gaps());
    }

    #[test]
    fn reports_a_truncated_final_frame() {
        let bytes = [frame(3, 9, 0, false), frame(3, 9, 0, false)].concat();
        let timeline = scan(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(timeline.frames.len(), 1);
        assert!(
            timeline
                .events
                .iter()
                .any(|event| event.kind == EventKind::TruncatedFrame)
        );
    }

    #[test]
    fn stops_at_a_format_change() {
        let bytes = [
            frame(3, 9, 0, false),
            frame(3, 9, 0, false),
            frame(2, 2, 0, false),
            frame(2, 2, 0, false),
        ]
        .concat();
        let timeline = scan(&bytes).unwrap();
        assert_eq!(timeline.frames.len(), 2);
        assert!(
            timeline
                .events
                .iter()
                .any(|event| event.kind == EventKind::FormatChange)
        );
    }

    #[test]
    fn rejects_random_bytes() {
        assert!(scan(&[0u8; 4096]).is_err());
    }
}
