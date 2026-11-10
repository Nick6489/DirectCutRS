use std::{io, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    version: u8,
    pub rate: u32,
    pub channels: u8,
}

impl Format {
    pub fn samples(self) -> usize {
        if self.version == 3 { 1152 } else { 576 }
    }

    fn side_len(self) -> usize {
        match (self.version, self.channels) {
            (3, 1) => 17,
            (3, _) => 32,
            (_, 1) => 9,
            _ => 17,
        }
    }

    fn reservoir(self) -> usize {
        if self.version == 3 { 511 } else { 255 }
    }

    fn frame_len(self, bitrate: usize, padding: bool) -> usize {
        const MPEG1: [usize; 15] = [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ];
        const LSF: [usize; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
        let bits = if self.version == 3 {
            144_000 * MPEG1[bitrate]
        } else {
            72_000 * LSF[bitrate]
        };
        bits / self.rate as usize + usize::from(padding)
    }
}

#[derive(Debug)]
pub struct Frame {
    header: [u8; 4],
    side: Box<[u8]>,
    data: Box<[u8]>,
}

pub type Frames = Vec<Arc<Frame>>;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn header_format(h: [u8; 4]) -> Result<Format> {
    let version = (h[1] >> 3) & 3;
    let rate = [44_100, 48_000, 32_000, 0][((h[2] >> 2) & 3) as usize];
    if h[0] != 255
        || h[1] & 0xe0 != 0xe0
        || version == 1
        || (h[1] >> 1) & 3 != 1
        || rate == 0
        || h[3] & 3 == 2
    {
        return Err(invalid("Expected a valid MPEG Layer III frame header.").into());
    }
    match h[2] >> 4 {
        0 => return Err(invalid("Free-format MP3 is unsupported.").into()),
        15 => return Err(invalid("Invalid MP3 bitrate index.").into()),
        _ => {}
    }
    Ok(Format {
        version,
        rate: rate
            >> match version {
                3 => 0,
                2 => 1,
                _ => 2,
            },
        channels: if h[3] >> 6 == 3 { 1 } else { 2 },
    })
}

fn part_lengths(side: &[u8], format: Format) -> usize {
    let mono = format.channels == 1;
    let (start, stride, count) = if format.version == 3 {
        (if mono { 18 } else { 20 }, 59, format.channels * 2)
    } else {
        (if mono { 9 } else { 10 }, 63, format.channels)
    };
    (0..count as usize)
        .map(|part| {
            let offset = start + stride * part;
            (offset..offset + 12).fold(0, |value, bit| {
                (value << 1) | usize::from((side[bit / 8] >> (7 - bit % 8)) & 1)
            })
        })
        .sum()
}

fn audio_bytes(mut bytes: &[u8]) -> Result<&[u8]> {
    while bytes.starts_with(b"ID3") {
        let header = bytes
            .get(..10)
            .ok_or_else(|| invalid("Truncated ID3 tag."))?;
        if header[6..10].iter().any(|size| *size >= 128) {
            return Err(invalid("Invalid ID3 tag size.").into());
        }
        let size = header[6..10]
            .iter()
            .fold(0, |size, byte| (size << 7) | usize::from(*byte));
        let footer = if header[3] == 4 && header[5] & 0x10 != 0 {
            10
        } else {
            0
        };
        bytes = bytes
            .get(10 + size + footer..)
            .ok_or_else(|| invalid("Truncated ID3 tag."))?;
    }
    if bytes.len() >= 128 && bytes[bytes.len() - 128..].starts_with(b"TAG") {
        bytes = &bytes[..bytes.len() - 128];
    }
    if bytes.len() >= 32 {
        let footer = &bytes[bytes.len() - 32..];
        if footer.starts_with(b"APETAGEX") {
            let size = u32::from_le_bytes(footer[12..16].try_into()?) as usize;
            let has_header = footer[23] & 0x80 != 0;
            let size = size
                .checked_add(if has_header { 32 } else { 0 })
                .filter(|size| (32..=bytes.len()).contains(size))
                .ok_or_else(|| invalid("Invalid APE tag size."))?;
            bytes = &bytes[..bytes.len() - size];
        }
    }
    Ok(bytes)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    pub skipped_bytes: usize,
    pub skipped_frames: usize,
}

struct FrameHeader {
    format: Format,
    bytes: [u8; 4],
    len: usize,
    side_start: usize,
}

fn complete_header(bytes: &[u8]) -> Option<FrameHeader> {
    let header: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    // Most bytes encountered during recovery are nowhere near an MP3 sync word.
    if header[0] != 0xff || header[1] & 0xe0 != 0xe0 {
        return None;
    }
    let format = header_format(header).ok()?;
    let len = format.frame_len((header[2] >> 4) as usize, header[2] & 2 != 0);
    let side_start = if header[1] & 1 == 0 { 6 } else { 4 };
    (len >= side_start + format.side_len() && len <= bytes.len()).then_some(FrameHeader {
        format,
        bytes: header,
        len,
        side_start,
    })
}

fn find_sync(bytes: &[u8]) -> Option<usize> {
    bytes.iter().enumerate().find_map(|(offset, &byte)| {
        if byte != 0xff {
            return None;
        }
        let first = complete_header(&bytes[offset..])?;
        let second = complete_header(&bytes[offset + first.len..])?;
        let rest = &bytes[offset + first.len + second.len..];
        (second.format == first.format
            && (rest.is_empty() || complete_header(rest).is_some_and(|h| h.format == first.format)))
        .then_some(offset)
    })
}

pub fn parse(bytes: &[u8]) -> Result<(Format, Frames, Recovery)> {
    let mut bytes = audio_bytes(bytes)?;
    let first_offset = if complete_header(bytes).is_some_and(|h| {
        let after = &bytes[h.len..];
        after.is_empty()
            || after.get(..4).is_some_and(|next| {
                header_format(next.try_into().unwrap()).is_ok_and(|format| format == h.format)
            })
    }) {
        0
    } else {
        find_sync(bytes).ok_or_else(|| invalid("No complete MP3 frame sequence found."))?
    };
    let mut recovery = Recovery {
        skipped_bytes: first_offset,
        skipped_frames: 0,
    };
    bytes = &bytes[first_offset..];
    let format = complete_header(bytes).unwrap().format;
    let mut frames = Frames::new();
    let mut reservoir = Vec::with_capacity(format.reservoir() + format.frame_len(14, true));
    let mut pending_end = 0;
    let mut stream_start = true;
    while let Some(header) = complete_header(bytes) {
        if header.format != format {
            return Err(
                invalid("The MP3 changes MPEG version, sample rate, or channel count.").into(),
            );
        }
        let after = &bytes[header.len..];
        let next_offset = if after.is_empty() || complete_header(after).is_some() {
            header.len
        } else {
            // If the next frame starts inside this one, this frame was cut short.
            // Search from its side information onward so we can still find that sync.
            find_sync(&bytes[header.side_start..])
                .map_or(bytes.len(), |offset| header.side_start + offset)
        };
        if next_offset < header.len {
            recovery.skipped_bytes += next_offset;
            bytes = &bytes[next_offset..];
            reservoir.clear();
            pending_end = 0;
            stream_start = true;
            continue;
        }

        let frame = &bytes[..header.len];
        let side_end = header.side_start + format.side_len();
        let side = &frame[header.side_start..side_end];
        let slot = &frame[side_end..];
        let back = if format.version == 3 {
            (usize::from(side[0]) << 1) | usize::from(side[1] >> 7)
        } else {
            usize::from(side[0])
        };
        let bits = part_lengths(side, format);
        let is_index = stream_start
            && bits == 0
            && (slot.starts_with(b"Xing")
                || slot.starts_with(b"Info")
                || frame.get(36..40) == Some(b"VBRI"));
        stream_start = false;

        let start = reservoir
            .len()
            .checked_sub(back)
            .filter(|&start| start >= pending_end);
        reservoir.extend_from_slice(slot);
        let range = start.map(|start| start..start + bits.div_ceil(8));
        if let Some(range) = range.filter(|range| range.end <= reservoir.len()) {
            pending_end = range.end;
            if !is_index {
                frames.push(Arc::new(Frame {
                    header: header.bytes,
                    side: side.into(),
                    data: reservoir[range].into(),
                }));
            }
        } else {
            // A frame with missing history may still hold bytes needed by later frames.
            recovery.skipped_frames += 1;
        }

        bytes = &bytes[next_offset..];
        if next_offset > header.len {
            recovery.skipped_bytes += next_offset - header.len;
            reservoir.clear();
            pending_end = 0;
            stream_start = true;
        } else {
            let discard = reservoir.len().saturating_sub(format.reservoir());
            reservoir.drain(..discard);
            pending_end = pending_end.saturating_sub(discard);
        }
    }
    if frames.is_empty() {
        return Err(invalid("No recoverable MP3 audio frames found.").into());
    }
    Ok((format, frames, recovery))
}
struct Layout {
    header: [u8; 4],
    slot: usize,
    start: usize,
    back: usize,
    end: usize,
}

/// Reschedule the existing scalefactors and Huffman bits into the output reservoir.
/// No PCM samples or encoder are involved. Raise a frame's transport bitrate only
/// when its original frame size cannot hold the data due by the end of that frame.
pub struct Packed {
    format: Format,
    frames: Frames,
    layouts: Vec<Layout>,
    index: usize,
    source: usize,
}

impl Packed {
    pub fn new(format: Format, frames: Frames) -> Result<Self> {
        if frames.is_empty() {
            return Err(invalid("There is no audio to save or play.").into());
        }
        let layouts =
            Self::plan(format, &frames, false).or_else(|_| Self::plan(format, &frames, true))?;
        Ok(Self {
            format,
            frames,
            layouts,
            index: 0,
            source: 0,
        })
    }

    fn plan(format: Format, frames: &[Arc<Frame>], max_capacity: bool) -> Result<Vec<Layout>> {
        let overhead = 4 + format.side_len();
        let mut layouts = Vec::with_capacity(frames.len());
        let mut slot = 0usize;
        let mut previous_end = 0;
        for (index, frame) in frames.iter().enumerate() {
            let start = previous_end.max(slot.saturating_sub(format.reservoir()));
            let data_end = start + frame.data.len();
            let mut header = frame.header;
            header[1] |= 1; // Rewritten side information uses a header without a CRC.
            let original_rate = if max_capacity {
                14
            } else {
                (header[2] >> 4) as usize
            };
            let padded = max_capacity || header[2] & 2 != 0;
            let (bitrate, padded, end) = (original_rate..=14)
                .map(|rate| (rate, padded))
                .chain((!padded).then_some((14, true)))
                .map(|(rate, pad)| (rate, pad, slot + format.frame_len(rate, pad) - overhead))
                .find(|(_, _, end)| *end >= data_end)
                .ok_or_else(|| {
                    invalid(format!(
                        "Frame {} needs more MP3 reservoir space. Move the edit boundary earlier.",
                        index + 1
                    ))
                })?;
            header[2] = (header[2] & 0x0d) | ((bitrate as u8) << 4) | (u8::from(padded) << 1);
            layouts.push(Layout {
                header,
                slot,
                start,
                back: slot - start,
                end,
            });
            slot = end;
            previous_end = data_end;
        }

        Ok(layouts)
    }

    pub fn xing_header(&self) -> Result<Vec<u8>> {
        let overhead = 4 + self.format.side_len();
        let bitrate = (1..=14)
            .find(|&rate| self.format.frame_len(rate, false) >= overhead + 116)
            .ok_or_else(|| invalid("Could not fit the MP3 seek index."))?;
        let mut index = vec![0; self.format.frame_len(bitrate, false)];
        index[..4].copy_from_slice(&self.layouts[0].header);
        index[2] = (index[2] & 0x0c) | (bitrate as u8) << 4;
        let stream_bytes =
            index.len() + self.layouts.last().unwrap().end + overhead * self.layouts.len();
        let index_len = index.len();
        index[overhead..overhead + 4].copy_from_slice(b"Xing");
        index[overhead + 4..overhead + 8].copy_from_slice(&7u32.to_be_bytes());
        index[overhead + 8..overhead + 12]
            .copy_from_slice(&u32::try_from(self.layouts.len())?.to_be_bytes());
        index[overhead + 12..overhead + 16]
            .copy_from_slice(&u32::try_from(stream_bytes)?.to_be_bytes());
        for i in 1..100 {
            let frame = self.layouts.len() * i / 100;
            let offset = index_len + self.layouts[frame].slot + overhead * frame;
            index[overhead + 16 + i] = ((offset as u64 * 256) / stream_bytes as u64) as u8;
        }
        Ok(index)
    }

    pub fn next_frame(&mut self, packet: &mut Vec<u8>) -> bool {
        let Some(layout) = self.layouts.get(self.index) else {
            return false;
        };
        let frame = &self.frames[self.index];
        self.index += 1;
        packet.clear();
        packet.extend_from_slice(&layout.header);
        packet.extend_from_slice(&frame.side);
        let side = &mut packet[4..];
        let format = self.format;

        if format.version == 3 {
            side[0] = (layout.back >> 1) as u8;
            side[1] = (side[1] & 0x7f) | (((layout.back & 1) as u8) << 7);
        } else {
            side[0] = layout.back as u8;
        }
        let overhead = packet.len();
        packet.resize(overhead + layout.end - layout.slot, 0);
        let slot_data = &mut packet[overhead..];
        while self.source < self.layouts.len() && self.layouts[self.source].start < layout.end {
            let from = self.layouts[self.source].start;
            let data = &self.frames[self.source].data;
            let to = from + data.len();
            let overlap_start = from.max(layout.slot);
            let overlap_end = to.min(layout.end);
            if overlap_start < overlap_end {
                slot_data[overlap_start - layout.slot..overlap_end - layout.slot]
                    .copy_from_slice(&data[overlap_start - from..overlap_end - from]);
            }
            if to > layout.end {
                break;
            }
            self.source += 1;
        }
        true
    }
}

pub fn can_start(format: Format, frame: &Frame) -> bool {
    frame.data.len() <= format.frame_len(14, true) - 4 - format.side_len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_bits(side: &mut [u8], offset: usize, value: usize) {
        for bit in 0..12 {
            let position = offset + bit;
            side[position / 8] |= (((value >> (11 - bit)) & 1) as u8) << (7 - position % 8);
        }
    }

    #[test]
    fn splice_moves_main_data_across_frame_boundaries() {
        let header = [0xff, 0xfb, 0x90, 0];
        let format = header_format(header).unwrap();
        let frame = |len, value| {
            let mut side = vec![0; format.side_len()];
            encode_bits(&mut side, 20, len * 8);
            Arc::new(Frame {
                header,
                side: side.into(),
                data: vec![value; len].into(),
            })
        };
        let frames = vec![frame(150, 0xab), frame(470, 0xcd), frame(270, 0xef)];
        let mut packed = Packed::new(format, frames.clone()).unwrap();
        let mut bytes = packed.xing_header().unwrap();
        let mut packet = Vec::new();
        while packed.next_frame(&mut packet) {
            bytes.extend_from_slice(&packet);
        }
        let (format_out, output, recovery) = parse(&bytes).unwrap();
        assert_eq!(format_out, format);
        assert_eq!(recovery, Recovery::default());
        assert_eq!(output.len(), frames.len());
        for (expected, actual) in frames.iter().zip(&output) {
            assert_eq!(expected.data, actual.data);
            assert_eq!(
                part_lengths(&expected.side, format),
                part_lengths(&actual.side, format)
            );
        }
        assert_ne!(output[1].side[0], 0);
    }

    #[test]
    fn rejects_truncated_frames_and_reservoir_underflow() {
        let header = [0xff, 0xfb, 0x90, 0];
        let format = header_format(header).unwrap();
        let mut bytes = vec![0; format.frame_len(9, false)];
        bytes[..4].copy_from_slice(&header);
        assert!(parse(&bytes[..bytes.len() - 1]).is_err());
        bytes[4] = 1;
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn recovers_after_a_mid_frame_start_and_a_short_frame() {
        let header = [0xff, 0xfb, 0xb0, 0];
        let format = header_format(header).unwrap();
        let frame = |back: usize, data: u8| {
            let mut bytes = vec![0; format.frame_len(11, false)];
            bytes[..4].copy_from_slice(&header);
            bytes[4] = (back >> 1) as u8;
            bytes[5] = ((back & 1) << 7) as u8;
            encode_bits(&mut bytes[4..36], 20, 8);
            bytes[36] = data;
            bytes
        };
        let mut bytes = b"partial".to_vec();
        let mut first = frame(476, 0xaa);
        let last = first.len() - 1;
        first[last] = 0xab;
        bytes.extend(first);
        bytes.extend(frame(1, 0));
        bytes.extend(frame(0, 0xcd));
        bytes.extend_from_slice(&frame(0, 0xff)[..352]);
        bytes.extend(frame(0, 0xef));
        bytes.extend(frame(0, 0x12));
        bytes.extend(frame(0, 0x34));
        bytes.extend_from_slice(b"tail");
        let (actual, frames, recovery) = parse(&bytes).unwrap();
        assert_eq!(actual, format);
        assert_eq!(
            recovery,
            Recovery {
                skipped_bytes: 7 + 352 + 4,
                skipped_frames: 1
            }
        );
        let recovered: Vec<_> = frames.iter().map(|frame| frame.data[0]).collect();
        assert_eq!(recovered, [0xab, 0xcd, 0xef, 0x12, 0x34]);
    }

    #[test]
    #[ignore = "requires generated MP3 fixtures in DIRECTCUT_TEST_FIXTURES"]
    fn splice_real_streams() {
        let directory =
            std::path::PathBuf::from(std::env::var_os("DIRECTCUT_TEST_FIXTURES").unwrap());
        for (left, right) in [
            ("cbr", "vbr"),
            ("lsf", "lsf"),
            ("mpeg25", "mpeg25"),
            ("crc", "cbr"),
        ] {
            let read = |name| {
                parse(&std::fs::read(directory.join(format!("{name}.mp3"))).unwrap()).unwrap()
            };
            let (format, a, _) = read(left);
            let (format_b, b, _) = read(right);
            assert_eq!(format, format_b);
            let clip: Frames = a[4..14]
                .iter()
                .chain(&b[20..42])
                .chain(&a[52..60])
                .cloned()
                .collect();
            let output = directory.join(format!("out-{left}.mp3"));
            crate::audio::export(&output, format, clip.clone()).unwrap();
            let (actual_format, actual, recovery) = parse(&std::fs::read(output).unwrap()).unwrap();
            assert_eq!(actual_format, format);
            assert_eq!(recovery, Recovery::default());
            assert_eq!(actual.len(), clip.len());
            for (expected, frame) in clip.iter().zip(&actual) {
                assert_eq!(expected.data, frame.data);
                // Exclude just the repacked reservoir pointer.
                let mut side = frame.side.clone();
                side[0] = expected.side[0];
                if format.version == 3 {
                    side[1] = (side[1] & 0x7f) | (expected.side[1] & 0x80);
                }
                assert_eq!(expected.side, side);
            }
        }
    }
}
