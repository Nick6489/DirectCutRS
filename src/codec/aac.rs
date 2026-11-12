//! ADTS AAC frame walking.
//!
//! An ADTS frame is a 7-byte transport header followed by one raw AAC frame
//! (CRC-protected and multi-block ADTS are rejected, the same limits
//! symphonia's own reader applies). Frames are self-contained, so a lossless
//! splice copies frames byte-exactly; the export decodes each one through
//! symphonia's AAC decoder when the stream's profile is decodable.
//!
//! The walk is strict: no resynchronisation after a damaged frame, and a
//! header that changes the sample rate or channel configuration stops the
//! import, the same rule the MP3 importer applies to real format changes.

use std::sync::Arc;

use super::{Events, Frame, Info, Timeline};
use crate::mp3::{EventKind, Result};

/// Sample rates by ADTS `sampling_frequency_index`; indexes 13 and 14 are
/// reserved and 15 is forbidden.
const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// Channel counts by ADTS configuration; 0 means unknown in the stream.
const CHANNELS: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 8];

const HEADER_LEN: usize = 7;

/// An ADTS frame header. Parsing is shared with [`super::sniff`], which
/// recognises ADTS before any MPEG audio header rules apply.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Header {
    profile: u8,
    frequency_index: u8,
    rate: u32,
    channels: u8,
    /// Total frame length, header included.
    len: usize,
}

impl Header {
    /// Also used by [`super::sniff`] to recognise ADTS before any MPEG audio
    /// header check applies.
    pub(super) fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER_LEN {
            return None;
        }
        let (b0, b1, b2, b3, b4, b5, b6) = (
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
        );
        if b0 != 0xFF || b1 != 0xF1 {
            return None;
        }
        let frequency_index = (b2 >> 2) & 0xF;
        if frequency_index >= 13 {
            return None;
        }
        // The channel configuration spans byte 2's lowest bit and byte 3's top
        // two bits.
        let channel_configuration = ((b2 & 1) << 2) | ((b3 >> 6) & 3);
        if channel_configuration == 0 {
            return None;
        }
        // Two header bits carry the length's top bits, then eight, then three.
        let len = ((u16::from(b3) & 3) << 11) | (u16::from(b4) << 3) | u16::from(b5 >> 5);
        if len < HEADER_LEN as u16 || len > 16 * 1024 {
            return None;
        }
        // Only single-block ADTS is supported, matching symphonia's reader.
        if b6 & 3 != 0 {
            return None;
        }
        Some(Self {
            profile: b2 >> 6,
            frequency_index,
            rate: SAMPLE_RATES[frequency_index as usize],
            channels: CHANNELS[channel_configuration as usize],
            len: len as usize,
        })
    }

    fn info(self, decodable: bool) -> Info {
        Info::Aac {
            profile: self.profile,
            rate: self.rate,
            channels: self.channels,
            decodable,
        }
    }

    /// The format description and total byte length for format detection.
    /// Decodability is judged from the header's profile and channel count.
    pub(super) fn parts(self) -> (Info, usize) {
        (self.info(self.profile == 1 && self.channels <= 2), self.len)
    }

    fn matches(self, other: Self) -> bool {
        self.profile == other.profile
            && self.frequency_index == other.frequency_index
            && self.channels == other.channels
    }
}

/// Walk the whole file the same way [`super::mp2`] does: the first frame is
/// verified by a compatible successor, everything before it is leading
/// garbage, and the walk stops at the first damaged, truncated, or
/// format-changing frame.
pub fn scan(bytes: &[u8]) -> Result<Timeline> {
    let mut events = Events::new();
    let Some((offset, first)) = verified_start(bytes) else {
        return Err("No ADTS AAC frame sequence was found in the file.".into());
    };
    if offset > 0 {
        events.push(
            EventKind::LeadingGarbage,
            0,
            offset,
            0,
            "bytes before the first verified ADTS frame",
        );
    }
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
                "the AAC profile, sample rate, or channel configuration changed",
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
    let info = first.info(crate::audio::aac_decodable(&first.info(false), &frames));
    let (events, recovery, suppressed) = events.finish(0);
    Ok(Timeline {
        info,
        frames,
        recovery,
        events,
        suppressed_events: suppressed,
    })
}

/// Find the first byte offset that starts an ADTS frame followed by a
/// compatible ADTS frame.
fn verified_start(bytes: &[u8]) -> Option<(usize, Header)> {
    let mut position = 0;
    while position + HEADER_LEN <= bytes.len() {
        if let Some(header) = Header::parse(&bytes[position..]) {
            let next = position + header.len;
            if next + HEADER_LEN <= bytes.len()
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
    use super::{HEADER_LEN, Header, scan};
    use crate::mp3::EventKind;

    /// Build one complete ADTS frame: MPEG-4, no CRC, one raw data block,
    /// with zero payload beyond the header.
    fn frame(
        profile: u8,
        frequency_index: u8,
        channel_configuration: u8,
        payload: usize,
    ) -> Vec<u8> {
        let len = (HEADER_LEN + payload) as u16;
        let mut bytes = vec![
            0xFFu8,
            0xF1,
            ((profile & 3) << 6)
                | ((frequency_index & 0xF) << 2)
                | ((channel_configuration >> 2) & 1),
            ((channel_configuration & 3) << 6) | (((len >> 11) as u8) & 3),
            ((len >> 3) & 0xFF) as u8,
            (((len & 7) as u8) << 5) | 0b1_1111,
            0x00,
        ];
        bytes.resize(len as usize, 0);
        bytes
    }

    #[test]
    fn parses_an_adts_sequence() {
        let bytes = [frame(1, 4, 2, 0), frame(1, 4, 2, 0), frame(1, 4, 2, 0)].concat();
        let timeline = scan(&bytes).unwrap();
        assert_eq!(timeline.frames.len(), 3);
        assert_eq!(timeline.info.rate(), 44_100);
        assert_eq!(timeline.info.channels(), 2);
        assert_eq!(timeline.info.samples(), 1024);
        assert!(timeline.events.is_empty());
    }

    #[test]
    fn keeps_the_original_bytes() {
        let mut frame = frame(1, 4, 2, 16);
        for (index, byte) in frame.iter_mut().enumerate().skip(HEADER_LEN) {
            *byte = (index as u8) ^ 0x5A;
        }
        let frame_len = frame.len();
        let bytes = [frame.clone(), frame.clone()].concat();
        let timeline = scan(&bytes).unwrap();
        assert_eq!(timeline.frames.len(), 2);
        for (index, kept) in timeline.frames.iter().enumerate() {
            assert_eq!(
                kept.as_bytes().unwrap(),
                &bytes[index * frame_len..(index + 1) * frame_len],
                "frame {index} bytes must round-trip exactly"
            );
        }
    }

    #[test]
    fn rejects_reserved_rates_and_unknown_channels() {
        assert!(scan(&[frame(1, 15, 2, 0), frame(1, 15, 2, 0)].concat()).is_err());
        assert!(scan(&[frame(1, 4, 0, 0), frame(1, 4, 0, 0)].concat()).is_err());
    }

    #[test]
    fn rejects_multi_block_frames() {
        let mut bytes = frame(1, 4, 2, 0);
        bytes[6] |= 1;
        let two = [bytes.clone(), bytes].concat();
        assert!(scan(&two).is_err());
    }

    #[test]
    fn stops_at_a_format_change() {
        let bytes = [
            frame(1, 4, 2, 0),
            frame(1, 4, 2, 0),
            frame(1, 7, 1, 0),
            frame(1, 7, 1, 0),
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
    fn reports_a_truncated_final_frame() {
        // The final frame carries a payload, so truncation leaves a valid
        // header whose body ends early — the true truncated-frame case.
        let bytes = [frame(1, 4, 2, 10), frame(1, 4, 2, 10)].concat();
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
    fn header_length_fields_round_trip() {
        for payload in [0usize, 1, 100, 500, 4000] {
            let bytes = frame(1, 4, 2, payload);
            let header = Header::parse(&bytes).expect("header must parse");
            assert_eq!(header.len, bytes.len(), "payload {payload}");
        }
    }
}
