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

    #[cfg(test)]
    pub(crate) fn test(rate: u32, channels: u8) -> Self {
        Self {
            version: 3,
            rate,
            channels,
        }
    }

    /// MPEG audio version: 3 = MPEG-1, 2 = MPEG-2, 0 = MPEG-2.5.
    pub fn version(self) -> u8 {
        self.version
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
    /// Byte offset of this frame in the file it was imported from.
    #[allow(dead_code)]
    source: usize,
    /// Length of this frame in the file it was imported from.
    #[allow(dead_code)]
    length: usize,
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

/// Every event worth telling an archivist about, with exact byte offsets into the
/// file the caller supplied. Offsets are never converted to time here: a byte
/// range does not prove how much audio, if any, it once held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// A tag or other non-audio block at the file boundary. Informational.
    Metadata,
    /// Bytes before the first verified frame sequence.
    LeadingGarbage,
    /// Stray bytes between frames.
    Garbage,
    /// A frame header or frame body that ends before its declared length.
    TruncatedFrame,
    /// A header at a frame boundary that cannot belong to the stream.
    InvalidHeader,
    /// A frame whose main data needed reservoir bytes that were not available.
    ReservoirUnderflow,
    /// The stream changes MPEG version, sample rate, or channel count.
    FormatChange,
    /// A Xing/Info/VBRI index frame. It carries no audio and is rebuilt on save.
    Index,
}

impl EventKind {
    /// Whether this event means audio or stream structure was lost.
    pub fn is_problem(self) -> bool {
        !matches!(self, EventKind::Metadata | EventKind::Index)
    }

    pub fn label(self) -> &'static str {
        match self {
            EventKind::Metadata => "metadata",
            EventKind::LeadingGarbage => "leading bytes",
            EventKind::Garbage => "stray bytes",
            EventKind::TruncatedFrame => "truncated frame",
            EventKind::InvalidHeader => "invalid frame header",
            EventKind::ReservoirUnderflow => "missing reservoir data",
            EventKind::FormatChange => "format change",
            EventKind::Index => "stream index",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryEvent {
    pub kind: EventKind,
    /// First byte offset in the original input.
    pub start: usize,
    /// One past the last byte offset in the original input.
    pub end: usize,
    /// Observed frames dropped because of this event, when knowable. A byte gap
    /// can hide any number of frames, so it reports zero rather than guessing.
    pub frames: usize,
    pub detail: String,
}

impl RecoveryEvent {
    pub fn bytes(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn describe(&self) -> String {
        let mut text = format!(
            "{}..{} ({} bytes) {}",
            self.start,
            self.end,
            self.bytes(),
            self.kind.label()
        );
        if self.frames > 0 {
            text.push_str(&format!(", {} frame(s)", self.frames));
        }
        if !self.detail.is_empty() {
            text.push_str(": ");
            text.push_str(&self.detail);
        }
        text
    }
}

/// A bounded, merged list of recovery events. Merging adjacent events of the same
/// kind keeps reports readable; the cap keeps pathological files from exhausting
/// memory or making the report unbounded.
const EVENT_LIMIT: usize = 512;

#[derive(Default)]
struct EventLog {
    events: Vec<RecoveryEvent>,
    suppressed: usize,
}

impl EventLog {
    fn push(
        &mut self,
        kind: EventKind,
        start: usize,
        end: usize,
        frames: usize,
        detail: impl Into<String>,
    ) {
        let end = end.max(start);
        if end == start && frames == 0 {
            return;
        }
        let detail = detail.into();
        if let Some(last) = self.events.last_mut()
            && last.kind == kind
            && last.detail == detail
            && last.end == start
        {
            last.end = end;
            last.frames += frames;
            return;
        }
        if self.events.len() < EVENT_LIMIT {
            self.events.push(RecoveryEvent {
                kind,
                start,
                end,
                frames,
                detail,
            });
        } else if self.events.last_mut().is_some_and(|last| last.kind == kind) {
            // Keep a final event honest about the extra span even when details differ.
            let last = self.events.last_mut().unwrap();
            last.end = last.end.max(end);
            last.frames += frames;
            self.suppressed += 1;
        } else {
            self.suppressed += 1;
        }
    }
}

/// Compatibility counters. `skipped_frames` keeps its historical meaning: frames
/// dropped because their bit-reservoir history was unavailable. Frames observed
/// but too damaged to import are counted in [`RecoveryEvent::frames`] instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

struct AudioRegion<'a> {
    start: usize,
    data: &'a [u8],
    log: EventLog,
}

/// Split leading and trailing tag data from the MPEG stream, recording what was
/// found without refusing to look for audio when a tag is malformed. A bogus tag
/// must not make a recoverable recording unopenable.
fn audio_region(bytes: &[u8]) -> AudioRegion<'_> {
    let mut log = EventLog::default();
    let mut start = 0;
    loop {
        let rest = &bytes[start..];
        if !rest.starts_with(b"ID3") {
            break;
        }
        let Some(header) = rest.get(..10) else {
            log.push(
                EventKind::Metadata,
                start,
                bytes.len(),
                0,
                "a truncated ID3v2 tag header was not copied",
            );
            return AudioRegion {
                start: bytes.len(),
                data: &[],
                log,
            };
        };
        let size_valid = header[6..10].iter().all(|size| *size < 128);
        let size = header[6..10]
            .iter()
            .fold(0, |size, byte| (size << 7) | usize::from(*byte));
        let footer = usize::from(header[3] == 4 && header[5] & 0x10 != 0) * 10;
        let total = 10usize.saturating_add(size).saturating_add(footer);
        let version = format!("ID3v2.{}", header[3]);
        if size_valid && total <= rest.len() {
            log.push(
                EventKind::Metadata,
                start,
                start + total,
                0,
                format!("{version} tag at the start of the file was not copied"),
            );
            start += total;
            continue;
        }
        log.push(
            EventKind::Metadata,
            start,
            start + 10,
            0,
            format!(
                "{version} tag has an invalid or truncated size; only its 10-byte header was skipped"
            ),
        );
        // Scan after the header instead of trusting the bogus size. Sync
        // verification still has to prove that a frame sequence follows.
        start += 10;
        break;
    }

    let mut end = bytes.len();
    if end >= 128 && bytes[end - 128..].starts_with(b"TAG") {
        log.push(
            EventKind::Metadata,
            end - 128,
            end,
            0,
            "ID3v1 tag at the end of the file was not copied",
        );
        end -= 128;
    }
    if end >= 32 {
        let footer = &bytes[end - 32..end];
        if footer.starts_with(b"APETAGEX") {
            let size = u32::from_le_bytes(footer[12..16].try_into().unwrap()) as usize;
            let has_header = footer[23] & 0x80 != 0;
            match size
                .checked_add(if has_header { 32 } else { 0 })
                .filter(|size| (32..=end).contains(size))
            {
                Some(total) => {
                    log.push(
                        EventKind::Metadata,
                        end - total,
                        end,
                        0,
                        "APEv2 tag at the end of the file was not copied",
                    );
                    end -= total;
                }
                None => log.push(
                    EventKind::Metadata,
                    end - 32,
                    end,
                    0,
                    "APEv2 footer has an invalid size; it was left in place",
                ),
            }
        }
    }
    AudioRegion {
        start,
        data: &bytes[start..end],
        log,
    }
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

/// A 4-byte MPEG Layer III header without requiring the frame body to be present.
fn header_at(bytes: &[u8]) -> Option<Format> {
    header_format(bytes.get(..4)?.try_into().ok()?).ok()
}

fn starts_stream(bytes: &[u8]) -> bool {
    let Some(first) = complete_header(bytes) else {
        return false;
    };
    let after = &bytes[first.len..];
    after.is_empty() || header_at(after).is_some_and(|format| format == first.format)
}

/// Find a byte offset whose header is followed by enough compatible headers to be
/// credible. A single random sync signature is not evidence of audio.
fn find_sync_of(bytes: &[u8], from: usize, wanted: Option<Format>) -> Option<usize> {
    let mut pos = from;
    while pos + 4 <= bytes.len() {
        if bytes[pos] != 0xff {
            pos += 1;
            continue;
        }
        let Some(first) = complete_header(&bytes[pos..]) else {
            pos += 1;
            continue;
        };
        let format = wanted.unwrap_or(first.format);
        if first.format != format {
            pos += 1;
            continue;
        }
        let second_start = pos + first.len;
        let Some(second) = complete_header(&bytes[second_start..]) else {
            pos += 1;
            continue;
        };
        if second.format != format {
            pos += 1;
            continue;
        }
        let rest = &bytes[second_start + second.len..];
        if rest.is_empty() || complete_header(rest).is_some_and(|h| h.format == format) {
            return Some(pos);
        }
        pos += 1;
    }
    None
}

fn find_sync(bytes: &[u8]) -> Option<usize> {
    find_sync_of(bytes, 0, None)
}

/// The result of scanning one file. `frames` is empty for [`inspect`], which
/// avoids copying all compressed data just to produce a report.
pub struct Scan {
    pub format: Format,
    pub frames: Frames,
    pub frame_count: usize,
    /// Retained frame lengths in the input, including frame headers and side data.
    pub retained_bytes: usize,
    pub input_len: usize,
    /// Offset of the first MPEG byte after leading metadata.
    pub audio_offset: usize,
    pub recovery: Recovery,
    pub events: Vec<RecoveryEvent>,
    pub suppressed_events: usize,
}

impl Scan {
    pub fn duration(&self) -> std::time::Duration {
        crate::audio::duration(self.format, self.frame_count)
    }

    /// True when imported audio has a gap, a format change, or a damaged frame.
    pub fn has_gaps(&self) -> bool {
        self.recovery.skipped_bytes > 0
            || self.recovery.skipped_frames > 0
            || self.events.iter().any(|event| {
                matches!(
                    event.kind,
                    EventKind::FormatChange
                        | EventKind::TruncatedFrame
                        | EventKind::InvalidHeader
                        | EventKind::ReservoirUnderflow
                )
            })
    }
}

/// Parse and retain every recoverable frame. Compatibility wrapper for callers
/// that only need the format, frames, and aggregate counters.
#[allow(dead_code)]
pub fn parse(bytes: &[u8]) -> Result<(Format, Frames, Recovery)> {
    let scan = scan(bytes)?;
    Ok((scan.format, scan.frames, scan.recovery))
}

/// Parse and retain every recoverable frame, with structured events.
pub fn scan(bytes: &[u8]) -> Result<Scan> {
    run(bytes, true)
}

/// Read-only analysis without retaining compressed frames.
pub fn inspect(bytes: &[u8]) -> Result<Scan> {
    run(bytes, false)
}

fn run(bytes: &[u8], keep: bool) -> Result<Scan> {
    let region = audio_region(bytes);
    let audio = region.data;
    let mut log = region.log;
    let absolute = |offset: usize| region.start + offset;

    let entry = if starts_stream(audio) {
        0
    } else {
        find_sync(audio).ok_or_else(|| invalid("No complete MP3 frame sequence found."))?
    };
    if entry > 0 {
        log.push(
            EventKind::LeadingGarbage,
            absolute(0),
            absolute(entry),
            0,
            "bytes before the first verified frame sequence were skipped",
        );
    }

    let mut pos = entry;
    let format = complete_header(&audio[pos..]).unwrap().format;
    let mut frames = Frames::new();
    let mut frame_count = 0usize;
    let mut retained_bytes = 0usize;
    let mut recovery = Recovery {
        skipped_bytes: entry,
        skipped_frames: 0,
    };
    let mut reservoir = Vec::with_capacity(format.reservoir() + format.frame_len(14, true));
    let mut pending_end = 0;
    let mut stream_start = true;

    while pos < audio.len() {
        let rest = &audio[pos..];
        let Some(header) = complete_header(rest) else {
            let plausible = header_at(rest).is_some();
            if let Some(found) = find_sync_of(rest, 1, Some(format)) {
                let kind = if plausible {
                    EventKind::InvalidHeader
                } else {
                    EventKind::Garbage
                };
                log.push(
                    kind,
                    absolute(pos),
                    absolute(pos + found),
                    usize::from(plausible),
                    if plausible {
                        "a damaged frame header was skipped; import resumed at a verified frame sequence"
                    } else {
                        "stray bytes were skipped; import resumed at a verified frame sequence"
                    },
                );
                recovery.skipped_bytes += found;
                reservoir.clear();
                pending_end = 0;
                stream_start = true;
                pos += found;
                continue;
            }
            let (kind, detail) = if plausible {
                (
                    EventKind::TruncatedFrame,
                    "an incomplete frame header or frame body at the end of the file was not imported",
                )
            } else {
                (
                    EventKind::Garbage,
                    "trailing bytes do not form a verified frame sequence and were not imported",
                )
            };
            log.push(
                kind,
                absolute(pos),
                absolute(audio.len()),
                usize::from(plausible),
                detail,
            );
            recovery.skipped_bytes += audio.len() - pos;
            break;
        };

        if header.format != format {
            // A single corrupted header byte can change version, rate, or channel
            // count. Look for a continuation of the format actually being imported
            // before deciding this is a real stream change.
            if let Some(found) = find_sync_of(rest, 1, Some(format)) {
                log.push(
                    EventKind::InvalidHeader,
                    absolute(pos),
                    absolute(pos + found),
                    1,
                    "a frame header of another format was skipped; import resumed at a verified frame sequence",
                );
                recovery.skipped_bytes += found;
                reservoir.clear();
                pending_end = 0;
                stream_start = true;
                pos += found;
                continue;
            }
            log.push(
                EventKind::FormatChange,
                absolute(pos),
                absolute(audio.len()),
                0,
                "the stream changes MPEG version, sample rate, or channel count; the remaining bytes were not imported",
            );
            recovery.skipped_bytes += audio.len() - pos;
            break;
        }

        // A complete-looking header whose declared length does not end at another
        // verified header, or whose declared length runs past EOF, is suspect.
        let after = &rest[header.len..];
        let next_offset = if after.is_empty() || complete_header(after).is_some() {
            header.len
        } else {
            find_sync_of(&rest[header.side_start..], 0, Some(format))
                .map_or(audio.len() - pos, |offset| header.side_start + offset)
        };

        if next_offset < header.len {
            // The next verified frame starts inside this frame's declared span.
            // The frame was cut short, so drop it rather than importing garbage.
            log.push(
                EventKind::TruncatedFrame,
                absolute(pos),
                absolute(pos + next_offset),
                1,
                "a short frame was skipped; import resumed at the next verified frame",
            );
            recovery.skipped_bytes += next_offset;
            reservoir.clear();
            pending_end = 0;
            stream_start = true;
            pos += next_offset;
            continue;
        }

        let frame = &rest[..header.len];
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
        if is_index {
            log.push(
                EventKind::Index,
                absolute(pos),
                absolute(pos + header.len),
                0,
                "a Xing, Info, or VBRI index frame was found; a new index is written on save",
            );
        }
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
                frame_count += 1;
                retained_bytes += header.len;
                if keep {
                    frames.push(Arc::new(Frame {
                        header: header.bytes,
                        side: side.into(),
                        data: reservoir[range].into(),
                        source: absolute(pos),
                        length: header.len,
                    }));
                }
            }
        } else {
            // A frame with missing history may still hold bytes needed by later frames.
            log.push(
                EventKind::ReservoirUnderflow,
                absolute(pos),
                absolute(pos + header.len),
                1,
                "an audio frame needed reservoir data that was not available; it was skipped",
            );
            recovery.skipped_frames += 1;
        }

        if next_offset > header.len {
            let skipped = &rest[header.len..next_offset];
            let (kind, frames, detail) = if header_at(skipped).is_some() {
                (
                    EventKind::TruncatedFrame,
                    usize::from(next_offset == audio.len() - pos),
                    "an incomplete frame was skipped at the end of the file",
                )
            } else if next_offset == audio.len() - pos {
                (
                    EventKind::Garbage,
                    0,
                    "trailing bytes do not form a verified frame sequence and were not imported",
                )
            } else {
                (
                    EventKind::Garbage,
                    0,
                    "stray bytes were skipped; import resumed at a verified frame sequence",
                )
            };
            log.push(
                kind,
                absolute(pos + header.len),
                absolute(pos + next_offset),
                frames,
                detail,
            );
            recovery.skipped_bytes += next_offset - header.len;
            reservoir.clear();
            pending_end = 0;
            stream_start = true;
        } else {
            let discard = reservoir.len().saturating_sub(format.reservoir());
            reservoir.drain(..discard);
            pending_end = pending_end.saturating_sub(discard);
        }
        pos += next_offset;
    }

    if frame_count == 0 {
        return Err(invalid("No recoverable MP3 audio frames found.").into());
    }

    let mut events = log.events;
    events.sort_by_key(|event| (event.start, event.end));
    Ok(Scan {
        format,
        frames,
        frame_count,
        retained_bytes,
        input_len: bytes.len(),
        audio_offset: absolute(entry),
        recovery,
        events,
        suppressed_events: log.suppressed,
    })
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

    fn test_header() -> [u8; 4] {
        [0xff, 0xfb, 0x90, 0]
    }

    fn lsf_header() -> [u8; 4] {
        // MPEG-2, Layer III, 64 kbps, 22.05 kHz, mono.
        [0xff, 0xf3, 0x84, 0xc0]
    }

    /// Build one parsable Layer III frame whose main data is `data`.
    fn frame(header: [u8; 4], back: usize, data: &[u8]) -> Vec<u8> {
        let format = header_format(header).unwrap();
        let len = format.frame_len((header[2] >> 4) as usize, header[2] & 2 != 0);
        let side_start = if header[1] & 1 == 0 { 6 } else { 4 };
        assert!(side_start + format.side_len() + data.len() <= len);
        let mut bytes = vec![0u8; len];
        bytes[..4].copy_from_slice(&header);
        {
            let side = &mut bytes[side_start..side_start + format.side_len()];
            if format.version == 3 {
                side[0] = (back >> 1) as u8;
                side[1] = ((back & 1) as u8) << 7;
            } else {
                side[0] = back as u8;
            }
            let mono = format.channels == 1;
            let (start, stride, count) = if format.version == 3 {
                (if mono { 18 } else { 20 }, 59, format.channels * 2)
            } else {
                (if mono { 9 } else { 10 }, 63, format.channels)
            };
            for part in 0..count as usize {
                encode_bits(side, start + stride * part, 0);
            }
            encode_bits(side, start, data.len() * 8);
        }
        let slot = side_start + format.side_len();
        bytes[slot..slot + data.len()].copy_from_slice(data);
        bytes
    }

    fn stream(header: [u8; 4], frames: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in 1..=frames {
            bytes.extend(frame(header, 0, &[value as u8]));
        }
        bytes
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
                source: 0,
                length: format.frame_len(9, false),
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
    fn reports_leading_garbage_with_absolute_offsets() {
        let mut bytes = b"not audio!".to_vec();
        bytes.extend(stream(test_header(), 5));
        let scan = scan(&bytes).unwrap();
        assert_eq!(scan.frame_count, 5);
        assert_eq!(scan.recovery.skipped_bytes, 10);
        assert_eq!(scan.audio_offset, 10);
        assert!(scan.events.iter().any(|event| {
            event.kind == EventKind::LeadingGarbage && event.start == 0 && event.end == 10
        }));
    }

    #[test]
    fn reports_garbage_between_frames_and_keeps_both_sides() {
        let format = header_format(test_header()).unwrap();
        let mut bytes = stream(test_header(), 3);
        let gap = 3 * format.frame_len(9, false);
        bytes.extend_from_slice(&[0x5a; 64]);
        bytes.extend(stream(test_header(), 3));
        let scan = scan(&bytes).unwrap();
        assert_eq!(scan.frame_count, 6);
        assert_eq!(scan.recovery.skipped_bytes, 64);
        assert!(scan.events.iter().any(|event| {
            event.kind == EventKind::Garbage && event.start == gap && event.end == gap + 64
        }));
    }

    #[test]
    fn resynchronizes_after_a_corrupt_frame_header() {
        let format = header_format(test_header()).unwrap();
        let len = format.frame_len(9, false);
        let mut bytes = stream(test_header(), 6);
        // Flip the MPEG version bits of frame 4, as a damaged header byte would.
        bytes[3 * len + 1] ^= 0x08;
        let scan = scan(&bytes).unwrap();
        assert_eq!(scan.frame_count, 5);
        assert_eq!(scan.recovery.skipped_bytes, len);
        let values: Vec<_> = scan.frames.iter().map(|frame| frame.data[0]).collect();
        assert_eq!(values, [1, 2, 3, 5, 6]);
        assert!(scan.events.iter().any(|event| {
            event.kind == EventKind::InvalidHeader
                && event.start == 3 * len
                && event.end == 4 * len
                && event.frames == 1
        }));
    }

    #[test]
    fn stops_at_a_real_format_change_without_joining_streams() {
        let format = header_format(test_header()).unwrap();
        let len = format.frame_len(9, false);
        let mut bytes = stream(test_header(), 4);
        bytes.extend(stream(lsf_header(), 6));
        let scan = scan(&bytes).unwrap();
        assert_eq!(scan.frame_count, 4);
        assert_eq!(scan.recovery.skipped_bytes, bytes.len() - 4 * len);
        assert!(
            scan.events
                .iter()
                .any(|event| event.kind == EventKind::FormatChange && event.start == 4 * len)
        );
        assert!(scan.has_gaps());
    }

    #[test]
    fn reports_a_truncated_final_frame() {
        let format = header_format(test_header()).unwrap();
        let len = format.frame_len(9, false);
        let mut bytes = stream(test_header(), 5);
        let partial = frame(test_header(), 0, &[9]);
        bytes.extend_from_slice(&partial[..200]);
        let scan = scan(&bytes).unwrap();
        assert_eq!(scan.frame_count, 5);
        assert_eq!(scan.recovery.skipped_bytes, 200);
        assert!(scan.events.iter().any(|event| {
            event.kind == EventKind::TruncatedFrame
                && event.start == 5 * len
                && event.end == bytes.len()
                && event.frames == 1
        }));
    }

    #[test]
    fn reports_valid_id3_tags_without_blocking_audio() {
        let mut bytes = b"ID3\x03\x00\x00".to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 20]);
        bytes.extend_from_slice(&[0; 20]);
        bytes.extend(stream(test_header(), 4));
        let mut trailer = vec![0u8; 128];
        trailer[..3].copy_from_slice(b"TAG");
        bytes.extend_from_slice(&trailer);

        let scan = scan(&bytes).unwrap();
        assert_eq!(scan.frame_count, 4);
        assert_eq!(scan.audio_offset, 30);
        assert_eq!(scan.recovery, Recovery::default());
        assert!(!scan.has_gaps());
        assert!(
            scan.events
                .iter()
                .any(|event| event.kind == EventKind::Metadata && event.start == 0)
        );
        assert!(
            scan.events
                .iter()
                .any(|event| event.kind == EventKind::Metadata && event.end == bytes.len())
        );
    }

    #[test]
    fn recovers_audio_after_a_bogus_id3_size() {
        let mut bytes = b"ID3\x03\x00\x00".to_vec();
        // Each size byte is below 128, but the claimed tag is far larger than the file.
        bytes.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        bytes.extend_from_slice(&[0; 32]);
        let audio = stream(test_header(), 4);
        bytes.extend_from_slice(&audio);

        let scan = scan(&bytes).unwrap();
        assert_eq!(scan.frame_count, 4);
        assert_eq!(scan.audio_offset, 42);
        assert!(
            scan.events
                .iter()
                .any(|event| event.kind == EventKind::Metadata
                    && event.start == 0
                    && event.end == 10)
        );
        assert!(scan.events.iter().any(|event| {
            event.kind == EventKind::LeadingGarbage
                && event.start == 10
                && event.end == bytes.len() - audio.len()
        }));
    }

    #[test]
    fn frames_keep_their_source_offsets() {
        let format = header_format(test_header()).unwrap();
        let len = format.frame_len(9, false);
        let bytes = stream(test_header(), 3);
        let scan = scan(&bytes).unwrap();
        assert_eq!(scan.frames[0].source, 0);
        assert_eq!(scan.frames[1].source, len);
        assert_eq!(scan.frames[1].length, len);
        assert_eq!(scan.retained_bytes, 3 * len);
    }

    #[test]
    fn inspect_does_not_retain_frames() {
        let bytes = stream(test_header(), 4);
        let scan = inspect(&bytes).unwrap();
        assert_eq!(scan.frame_count, 4);
        assert!(scan.frames.is_empty());
    }

    #[test]
    fn mutated_and_random_inputs_do_not_panic() {
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        let base = stream(test_header(), 8);
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for case in 0..400 {
            let mut bytes = base.clone();
            match case % 5 {
                0 => {
                    let index = (next(&mut state) as usize) % bytes.len();
                    bytes[index] ^= (next(&mut state) as u8) | 1;
                }
                1 => {
                    let index = (next(&mut state) as usize) % (bytes.len() + 1);
                    let count = (next(&mut state) as usize) % 64;
                    bytes.splice(index..index, std::iter::repeat_n(0xff, count));
                }
                2 => {
                    let index = (next(&mut state) as usize) % bytes.len();
                    let count = (next(&mut state) as usize) % (bytes.len() - index + 1);
                    bytes.drain(index..index + count);
                }
                3 => {
                    bytes.truncate((next(&mut state) as usize) % (bytes.len() + 1));
                }
                _ => {
                    let len = (next(&mut state) as usize) % 4096;
                    bytes = (0..len).map(|_| next(&mut state) as u8).collect();
                }
            }
            // The contract is no panic, hang, or unbounded scan; either a
            // conservative error or a bounded report is acceptable.
            let _ = scan(&bytes);
            let _ = inspect(&bytes);
        }
    }

    #[test]
    #[ignore = "requires generated corruption fixtures in DIRECTCUT_TEST_CORRUPTIONS"]
    fn classifies_real_corruptions() {
        let directory = std::path::PathBuf::from(
            std::env::var_os("DIRECTCUT_TEST_CORRUPTIONS")
                .expect("set DIRECTCUT_TEST_CORRUPTIONS to target\\corruptions"),
        );
        let case = |name: &str| {
            scan(&std::fs::read(directory.join(name)).unwrap())
                .unwrap_or_else(|error| panic!("{name}: {error}"))
        };

        let truncated = case("cbr-truncated-final.mp3");
        assert!(
            truncated
                .events
                .iter()
                .any(|event| event.kind == EventKind::TruncatedFrame)
        );
        assert!(truncated.has_gaps());

        let missing = case("cbr-missing-start.mp3");
        assert!(
            missing
                .events
                .iter()
                .any(|event| event.kind == EventKind::LeadingGarbage)
        );

        let inserted = case("cbr-garbage-insert.mp3");
        assert!(
            inserted
                .events
                .iter()
                .any(|event| event.kind == EventKind::Garbage)
        );

        let bad_header = case("cbr-bad-header.mp3");
        assert!(
            bad_header
                .events
                .iter()
                .any(|event| event.kind == EventKind::InvalidHeader)
        );

        let changed = case("cbr-format-change.mp3");
        assert!(
            changed
                .events
                .iter()
                .any(|event| event.kind == EventKind::FormatChange)
        );

        let bogus = case("cbr-id3-bogus.mp3");
        assert!(
            bogus
                .events
                .iter()
                .any(|event| event.kind == EventKind::Metadata)
        );
        assert!(bogus.frame_count > 200);

        let id3v1 = case("cbr-id3v1.mp3");
        assert_eq!(id3v1.recovery, Recovery::default());
        assert!(
            id3v1
                .events
                .iter()
                .any(|event| event.kind == EventKind::Metadata)
        );

        // Whole frames removed at a clean boundary are indistinguishable from a
        // shorter recording. The scan must not invent a gap it cannot prove.
        let whole = case("cbr-whole-frames-removed.mp3");
        assert!(!whole.has_gaps());
        assert!(whole.frame_count < id3v1.frame_count);

        for invalid in ["empty.mp3", "garbage.mp3"] {
            let bytes = std::fs::read(directory.join(invalid)).unwrap();
            assert!(inspect(&bytes).is_err(), "{invalid} should be rejected");
        }
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
