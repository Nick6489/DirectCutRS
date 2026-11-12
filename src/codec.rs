//! The codec layer: shared timeline vocabulary and the format registry.
//!
//! DirectCut RS edits by moving whole frames and never decodes and re-encodes
//! a lossy codec, so what "lossless" means depends on the loaded format. Every
//! capability statement in the interface comes from here, so the wording stays
//! identical in F6, the Help dialog, and analysis reports. See
//! `docs/format-expansion-plan.md` for the full capability model.
//!
//! A [`Timeline`] is the format-neutral view of one loaded file: an [`Info`]
//! describing rate, channels, and samples per frame, plus a vector of frames.
//! MPEG Layer III frames carry parsed main data because a splice may have to
//! repack the bit reservoir; the other formats supported so far (MP2, ADTS
//! AAC) have self-contained frames that are copied byte-exactly.

pub mod aac;
pub mod mp2;

use std::sync::Arc;

use crate::mp3::{self, Result};

/// Which codec a loaded timeline uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mp3,
    Mp2,
    Aac,
}

impl Kind {
    /// The human name used in activity lines and dialogs.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Mp3 => "MP3",
            Kind::Mp2 => "MP2",
            Kind::Aac => "ADTS AAC",
        }
    }

    /// The canonical file extension for this kind.
    pub fn extension(self) -> &'static str {
        match self {
            Kind::Mp3 => "mp3",
            Kind::Mp2 => "mp2",
            Kind::Aac => "aac",
        }
    }
}

/// What a format's timeline can be edited with, losslessly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Cut, delete, crop, paste, duplicate, and insert move whole frames:
    /// nothing is decoded or re-encoded, so retained audio is copied
    /// bit-exact from the source.
    pub frame_splice: bool,
    /// Gain, fade, and normalization change decoded samples. Bit-exact only
    /// where decode and encode round-trip losslessly (FLAC, ALAC, PCM);
    /// refused for lossy codecs rather than done silently lossy.
    pub sample_transform: bool,
}

impl Capabilities {
    /// The one-line disclosure shown in F6 and in analysis reports.
    pub fn summary(&self) -> String {
        match (self.frame_splice, self.sample_transform) {
            (true, true) => {
                "Lossless: cut, delete, crop, paste, and insert move whole frames. Fades, gain, and normalization are bit-exact.".into()
            }
            (true, false) => {
                "Lossless: cut, delete, crop, paste, and insert move whole frames. Fades, gain, and normalization are refused: they cannot be bit-exact for this format.".into()
            }
            (false, _) => "No lossless editing is available for this format.".into(),
        }
    }
}

/// MPEG Layer III: frames share the bit reservoir, so splices repack it, but a
/// fade would have to decode and re-encode, which loses a coding generation.
pub const MP3: Capabilities = Capabilities {
    frame_splice: true,
    sample_transform: false,
};

/// The format description of one loaded timeline. `Info::Mp3` carries the
/// parsed MPEG Layer III header fields the reservoir repacking needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Info {
    Mp3 {
        format: mp3::Format,
    },
    /// MPEG Layer II frames are self-contained and always hold 1152 samples.
    Mp2 {
        rate: u32,
        channels: u8,
    },
    /// ADTS AAC. `decodable` records whether this build can decode the
    /// stream's profile (AAC-LC, mono or stereo). When it cannot — for
    /// example HE-AAC/SBR — saving validates structure only, and F6 says so.
    Aac {
        profile: u8,
        rate: u32,
        channels: u8,
        decodable: bool,
    },
}

impl Info {
    pub fn kind(&self) -> Kind {
        match self {
            Info::Mp3 { .. } => Kind::Mp3,
            Info::Mp2 { .. } => Kind::Mp2,
            Info::Aac { .. } => Kind::Aac,
        }
    }

    pub fn name(&self) -> &'static str {
        self.kind().name()
    }

    pub fn extension(&self) -> &'static str {
        self.kind().extension()
    }

    /// The file-dialog label for this kind, such as `AAC audio`.
    pub fn audio_label(&self) -> String {
        self.extension().to_ascii_uppercase() + " audio"
    }

    pub fn rate(&self) -> u32 {
        match self {
            Info::Mp3 { format } => format.rate,
            Info::Mp2 { rate, .. } => *rate,
            Info::Aac { rate, .. } => *rate,
        }
    }

    pub fn channels(&self) -> u8 {
        match self {
            Info::Mp3 { format } => format.channels,
            Info::Mp2 { channels, .. } => *channels,
            Info::Aac { channels, .. } => *channels,
        }
    }

    /// Decoded samples one frame contributes to the timeline.
    pub fn samples(&self) -> usize {
        match self {
            Info::Mp3 { format } => format.samples(),
            Info::Mp2 { .. } => 1152,
            Info::Aac { .. } => 1024,
        }
    }

    pub fn capabilities(&self) -> Capabilities {
        MP3
    }

    /// The F6/report capability disclosure for this loaded stream.
    pub fn disclosure(&self) -> String {
        let mut text = self.capabilities().summary();
        if let Info::Aac {
            decodable: false, ..
        } = self
        {
            text.push_str(" This AAC stream could not be decoded by this build. Saving preserves the structurally validated ADTS frames.");
        }
        text
    }

    #[cfg(test)]
    pub fn test(rate: u32, channels: u8) -> Self {
        Info::Mp3 {
            format: mp3::Format::test(rate, channels),
        }
    }
}

/// One frame of the timeline.
pub enum Frame {
    /// An MPEG Layer III frame. Its main data and bit reservoir are parsed,
    /// because a splice may have to repack them.
    Mp3(Arc<mp3::Frame>),
    /// A self-contained frame: header and payload in one byte sequence,
    /// copied byte-exactly on splice.
    Data(Box<[u8]>),
}

pub type Frames = Vec<Arc<Frame>>;

impl Frame {
    /// The frame's complete byte sequence as it belongs in the output.
    pub fn as_bytes(&self) -> Result<&[u8]> {
        match self {
            Frame::Data(data) => Ok(data),
            Frame::Mp3(_) => Err("A timeline mixed frame kinds.".into()),
        }
    }

    /// The frame's full length in bytes, header and side information
    /// included. This is the transport length the timeline's average
    /// bitrate is measured over.
    pub fn len_bytes(&self) -> usize {
        match self {
            Frame::Data(data) => data.len(),
            Frame::Mp3(inner) => inner.length(),
        }
    }
}

/// The average transport bitrate of a timeline, in kilobits per second, or
/// `None` when it holds no frames. Frame lengths are summed over the
/// timeline's duration, so a VBR stream reports its average and a CBR
/// stream its constant rate.
pub fn average_kbps(info: Info, frames: &Frames) -> Option<f64> {
    if frames.is_empty() {
        return None;
    }
    let bytes: usize = frames.iter().map(|frame| frame.len_bytes()).sum();
    let seconds = frames.len() as f64 * info.samples() as f64 / f64::from(info.rate());
    (seconds > 0.0).then(|| bytes as f64 * 8.0 / 1000.0 / seconds)
}

/// The DirectCut clipboard: a format, its frames, and the file each run of
/// frames came from. The sources let a cue point named with `$file` keep
/// naming the file its audio came from as audio moves between windows,
/// the way the frames themselves keep their compressed bytes.
#[derive(Clone)]
pub struct Clipboard {
    pub info: Info,
    pub frames: Frames,
    /// Source file paths per run of frames, relative to the copied range:
    /// the first entry always starts at frame 0, and each entry covers the
    /// frames up to the next one. The paths distinguish files with the same name; the editor
    /// takes the file stem when displaying a $file title.
    pub sources: Vec<(usize, String)>,
}

/// Wrap parsed Layer III frames into the codec frame type.
pub fn frames_from_mp3(frames: mp3::Frames) -> Frames {
    frames
        .into_iter()
        .map(|frame| Arc::new(Frame::Mp3(frame)))
        .collect()
}

/// The format-neutral view of one loaded file.
pub struct Timeline {
    pub info: Info,
    pub frames: Frames,
    pub recovery: mp3::Recovery,
    pub events: Vec<mp3::RecoveryEvent>,
    pub suppressed_events: usize,
}

impl Timeline {
    /// True when imported audio has a gap, a format change, or a damaged
    /// frame.
    pub fn has_gaps(&self) -> bool {
        self.recovery.skipped_bytes > 0
            || self.recovery.skipped_frames > 0
            || self.events.iter().any(|event| {
                matches!(
                    event.kind,
                    mp3::EventKind::FormatChange
                        | mp3::EventKind::TruncatedFrame
                        | mp3::EventKind::InvalidHeader
                )
            })
    }

    pub fn from_mp3(scan: mp3::Scan) -> Self {
        Self {
            info: Info::Mp3 {
                format: scan.format,
            },
            frames: frames_from_mp3(scan.frames),
            recovery: scan.recovery,
            events: scan.events,
            suppressed_events: scan.suppressed_events,
        }
    }
}

/// Bounded event collection shared by the self-contained walkers. Once the
/// list is full, further events are only counted, so a hostile file cannot
/// blow up the report.
struct Events {
    list: Vec<mp3::RecoveryEvent>,
    recovery: mp3::Recovery,
    suppressed: usize,
}

impl Events {
    /// The same bound `mp3::scan` applies to its own event log.
    const LIMIT: usize = 64;

    fn new() -> Self {
        Self {
            list: Vec::new(),
            recovery: mp3::Recovery {
                skipped_bytes: 0,
                skipped_frames: 0,
            },
            suppressed: 0,
        }
    }

    fn push(
        &mut self,
        kind: mp3::EventKind,
        start: usize,
        end: usize,
        frames: usize,
        detail: impl Into<String>,
    ) {
        if self.list.len() < Self::LIMIT {
            self.list.push(mp3::RecoveryEvent {
                kind,
                start,
                end,
                frames,
                detail: detail.into(),
            });
        } else {
            self.suppressed += 1;
        }
        if kind.is_problem() {
            self.recovery.skipped_bytes += end.saturating_sub(start);
            self.recovery.skipped_frames += frames;
        }
    }

    fn finish(self, suppressed: usize) -> (Vec<mp3::RecoveryEvent>, mp3::Recovery, usize) {
        (
            self.list,
            self.recovery,
            self.suppressed.saturating_add(suppressed),
        )
    }
}

/// Look for the first byte position where a recognizable ADTS AAC or MPEG
/// Layer II header starts, within a bounded window. MP3 stays the legacy
/// default: when nothing else matches early, the reservoir parser decides and
/// owns the error.
const SNIFF_LIMIT: usize = 64 * 1024;

fn header_for(kind: Kind, tail: &[u8]) -> Option<(Info, usize)> {
    match kind {
        Kind::Mp3 => {
            let (format, len) = mp3::frame_at(tail)?;
            Some((Info::Mp3 { format }, len))
        }
        Kind::Mp2 => mp2::Header::parse(tail).map(|header| header.parts()),
        Kind::Aac => aac::Header::parse(tail).map(|header| header.parts()),
    }
}

/// Verify a pair of compatible headers before trusting a format guess.
fn sniff(bytes: &[u8]) -> Option<Kind> {
    let limit = bytes.len().min(SNIFF_LIMIT);
    (0..limit).find_map(|offset| {
        let tail = &bytes[offset..];
        let kind = kind_at(tail)?;
        let (info, length) = header_for(kind, tail)?;
        let (next, _) = header_for(kind, tail.get(length..)?)?;
        (info == next).then_some(kind)
    })
}

fn kind_at(tail: &[u8]) -> Option<Kind> {
    if tail.len() < 4 {
        return None;
    }
    // ADTS AAC first: its header pattern would be rejected by the MPEG audio
    // rules anyway (the layer bits are reserved there), so the two checks
    // cannot disagree about one file.
    if tail[0] == 0xFF && tail[1] == 0xF1 && aac::Header::parse(tail).is_some() {
        return Some(Kind::Aac);
    }
    if tail[0] != 0xFF || tail[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (tail[1] >> 3) & 3;
    if version == 1 {
        return None;
    }
    match (tail[1] >> 1) & 3 {
        1 => Some(Kind::Mp3),
        2 => Some(Kind::Mp2),
        _ => None,
    }
}

/// Load a timeline from raw file bytes. MP3 routes through the reservoir
/// parser; self-contained formats walk their own frames.
pub fn open(bytes: &[u8]) -> Result<Timeline> {
    let kind = sniff(bytes).unwrap_or(Kind::Mp3);
    match kind {
        Kind::Mp3 => Ok(Timeline::from_mp3(mp3::scan(bytes)?)),
        Kind::Mp2 => mp2::scan(bytes),
        Kind::Aac => aac::scan(bytes),
    }
}

/// The bytes handed to the decoder for one frame. ADTS frames carry a 7-byte
/// transport header the AAC decoder must not see.
pub fn packet_payload<'a>(info: &Info, frame: &'a [u8]) -> Result<&'a [u8]> {
    match info {
        Info::Aac { .. } => {
            if frame.len() < 7 {
                return Err("An ADTS frame is shorter than its header.".into());
            }
            Ok(&frame[7..])
        }
        _ => Ok(frame),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MPEG-1 Layer III, 44100 Hz stereo, 128 kbps, no padding, no CRC:
    /// sync, 0xFB (version 11, layer 01, no protection), 0x90 (bitrate
    /// index 9, sampling 00), 0x00 (stereo, no mode extension).
    const HEADER: [u8; 4] = [0xFF, 0xFB, 0x90, 0x00];

    fn frame_bytes(count: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        for _ in 0..count {
            bytes.extend_from_slice(&HEADER);
            bytes.resize(bytes.len() + 413, 0xA5);
        }
        bytes
    }

    #[test]
    fn mp3_splices_losslessly_and_refuses_sample_transforms() {
        assert!(MP3.frame_splice);
        assert!(!MP3.sample_transform);
        let summary = MP3.summary();
        assert!(summary.contains("whole frames"), "{summary}");
        assert!(summary.contains("refused"), "{summary}");
    }

    #[test]
    fn every_flag_combination_has_a_disclosure() {
        for frame_splice in [false, true] {
            for sample_transform in [false, true] {
                let capabilities = Capabilities {
                    frame_splice,
                    sample_transform,
                };
                assert!(!capabilities.summary().trim().is_empty());
            }
        }
    }

    #[test]
    fn info_reports_kind_dimensions() {
        let mp3 = Info::test(44_100, 2);
        assert_eq!(mp3.kind(), Kind::Mp3);
        assert_eq!(mp3.samples(), 1152);
        assert_eq!(mp3.name(), "MP3");
        assert_eq!(mp3.extension(), "mp3");
    }

    #[test]
    fn sniff_requires_two_compatible_headers() {
        let bytes = frame_bytes(2);
        assert_eq!(sniff(&bytes), Some(Kind::Mp3));
        assert_eq!(sniff(&bytes[..420]), None);
        let mut mismatched = bytes;
        mismatched[417..421].copy_from_slice(&[0xFF, 0xFB, 0x94, 0x00]);
        assert_eq!(sniff(&mismatched), None);
    }
}

#[cfg(test)]
mod real_files {
    //! Rehearsal on encoder-produced files. Runs when `DIRECTCUT_TEST_FIXTURES`
    //! points at a folder holding `cbr.mp3`, `cbr.mp2`, and `cbr.aac`,
    //! generated by `scripts/generate-fixtures.ps1`.

    use super::*;

    #[test]
    #[ignore = "requires generated fixtures in DIRECTCUT_TEST_FIXTURES"]
    fn opens_splices_and_saves_real_files() {
        for (name, expected_samples) in
            [("cbr.mp3", 1152usize), ("cbr.mp2", 1152), ("cbr.aac", 1024)]
        {
            let directory = std::env::var_os("DIRECTCUT_TEST_FIXTURES")
                .expect("set DIRECTCUT_TEST_FIXTURES to the generated fixture directory");
            let path = std::path::Path::new(&directory).join(name);
            let bytes =
                std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let mut timeline = open(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(timeline.info.samples(), expected_samples, "{name}");
            assert!(
                timeline.frames.len() >= 100,
                "{name}: only {} frame(s)",
                timeline.frames.len()
            );
            assert!(!timeline.has_gaps(), "{name}: unexpected gaps");
            if name.ends_with(".aac") {
                assert!(matches!(
                    timeline.info,
                    Info::Aac {
                        decodable: true,
                        ..
                    }
                ));
            }

            // A splice: delete everything before the second third, the same
            // operation the editor performs for a Delete Selection.
            let keep_from = timeline.frames.len() / 3 * 2;
            timeline.frames.drain(..keep_from);

            let directory = tempfile::tempdir().unwrap();
            let output = directory.path().join(name);
            crate::audio::export(&output, timeline.info, timeline.frames)
                .unwrap_or_else(|error| panic!("{name}: export: {error}"));
            let saved = std::fs::read(&output).unwrap();
            let reopened = open(&saved).unwrap_or_else(|error| panic!("{name}: reopen: {error}"));
            assert!(
                reopened.frames.len() >= 50,
                "{name}: reopen kept {} frame(s)",
                reopened.frames.len()
            );
            assert_eq!(reopened.info, timeline.info, "{name}: format changed");
        }
    }
}
