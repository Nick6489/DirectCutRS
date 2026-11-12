use std::{
    collections::HashMap,
    io,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use rodio::{
    OutputStream, Sink, Source,
    cpal::{
        self,
        traits::{DeviceTrait, HostTrait},
    },
};
use symphonia::core::{
    audio::{Channels, SampleBuffer},
    codecs::{
        CODEC_TYPE_AAC, CODEC_TYPE_MP2, CODEC_TYPE_MP3, CodecParameters, Decoder, DecoderOptions,
    },
    formats::Packet,
};

use crate::codec::{self, Frame, Frames, Info};
use crate::mp3::{Packed, Result};

pub(crate) fn decoder(info: &Info) -> Result<Box<dyn Decoder>> {
    let mut params = CodecParameters::new();
    match info {
        Info::Mp3 { .. } => {
            params.for_codec(CODEC_TYPE_MP3);
        }
        Info::Mp2 { .. } => {
            params.for_codec(CODEC_TYPE_MP2);
        }
        Info::Aac { rate, channels, .. } => {
            params
                .for_codec(CODEC_TYPE_AAC)
                .with_sample_rate(*rate)
                .with_channels(aac_channels(*channels)?);
        }
    }
    Ok(symphonia::default::get_codecs().make(&params, &DecoderOptions { verify: true })?)
}

/// symphonia's channel layout for an ADTS channel configuration; only mono
/// and stereo are decodable in this build.
fn aac_channels(channels: u8) -> Result<Channels> {
    match channels {
        1 => Ok(Channels::FRONT_LEFT),
        2 => Ok(Channels::FRONT_LEFT | Channels::FRONT_RIGHT),
        _ => Err("This AAC stream has more channels than this build can decode.".into()),
    }
}

/// Probe actual AAC-LC frames before advertising playback and decode validation.
pub fn aac_decodable(info: &Info, frames: &Frames) -> bool {
    let Info::Aac {
        profile: 1,
        channels: 1 | 2,
        ..
    } = info
    else {
        return false;
    };
    let Ok(mut decoder) = decoder(info) else {
        return false;
    };
    !frames.is_empty()
        && frames.iter().take(8).all(|frame| {
            frame
                .as_bytes()
                .and_then(|bytes| decode(decoder.as_mut(), info, bytes))
                .is_ok()
        })
}

pub(crate) fn decode<'a>(
    decoder: &'a mut dyn Decoder,
    info: &Info,
    frame: &[u8],
) -> Result<symphonia::core::audio::AudioBufferRef<'a>> {
    let packet = Packet::new_from_slice(
        0,
        0,
        info.samples() as u64,
        codec::packet_payload(info, frame)?,
    );
    let audio = decoder.decode(&packet)?;
    if audio.frames() != info.samples()
        || audio.spec().rate != info.rate()
        || audio.spec().channels.count() != usize::from(info.channels())
    {
        return Err(format!(
            "A {} frame did not decode to the expected audio format.",
            info.name()
        )
        .into());
    }
    Ok(audio)
}

/// Decode one frame and append its interleaved samples to `pcm`.
fn decode_into(
    decoder: &mut dyn Decoder,
    info: &Info,
    frame: &[u8],
    pcm: &mut Vec<f32>,
) -> Result<()> {
    let audio = decode(decoder, info, frame)?;
    let mut samples = SampleBuffer::new(audio.capacity() as u64, *audio.spec());
    samples.copy_interleaved_ref(audio);
    pcm.extend_from_slice(samples.samples());
    Ok(())
}

pub fn export(path: &Path, info: Info, frames: Frames) -> Result<()> {
    write_export(path, info, frames, true).map(|_| ())
}

/// What [`export_no_replace`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum NoReplace {
    /// The validated audio was written to this path.
    Written(PathBuf),
    /// The destination already existed and nothing was changed.
    Existed,
}

/// Export `frames` to `path` without ever replacing an existing file, and
/// report the path written. The destination name is claimed atomically just
/// before the validated audio commits, so a racing writer or a foreign file
/// can never be overwritten; a lost claim reports [`NoReplace::Existed`] and
/// the caller can retry with the next free name. The split feature uses this
/// to keep its promise that an existing file is never replaced.
pub fn export_no_replace(path: &Path, info: Info, frames: Frames) -> Result<NoReplace> {
    write_export(path, info, frames, false).map(|written| {
        if written {
            NoReplace::Written(path.to_path_buf())
        } else {
            NoReplace::Existed
        }
    })
}

fn write_export(path: &Path, info: Info, frames: Frames, replace: bool) -> Result<bool> {
    match info {
        Info::Mp3 { format } => export_mp3(path, format, mp3_frames(&frames)?, replace),
        _ => export_self_contained(path, info, frames, replace),
    }
}

/// Recover the parsed Layer III frames behind a timeline range. A timeline
/// only ever holds one kind, so the mismatch arm is unreachable by
/// construction. Saving packs everything; playback packs only the range it
/// consumes.
fn mp3_frames_range(
    frames: &Frames,
    range: std::ops::Range<usize>,
) -> Result<Vec<Arc<crate::mp3::Frame>>> {
    frames[range]
        .iter()
        .map(|frame| match &**frame {
            Frame::Mp3(inner) => Ok(Arc::clone(inner)),
            Frame::Data(_) => Err("A timeline mixed frame kinds.".into()),
        })
        .collect()
}

fn mp3_frames(frames: &Frames) -> Result<Vec<Arc<crate::mp3::Frame>>> {
    mp3_frames_range(frames, 0..frames.len())
}

/// Whether the timeline frame at `index` is a Layer III frame the reservoir
/// plan can start on. Playback only reaches here for an MP3 timeline, so a
/// data frame counts as unstartable.
fn can_start_at(format: crate::mp3::Format, frames: &Frames, index: usize) -> bool {
    match &*frames[index] {
        Frame::Mp3(inner) => crate::mp3::can_start(format, inner),
        Frame::Data(_) => false,
    }
}

fn export_mp3(
    path: &Path,
    format: crate::mp3::Format,
    frames: Vec<Arc<crate::mp3::Frame>>,
    replace: bool,
) -> Result<bool> {
    let info = Info::Mp3 { format };
    let mut packed = Packed::new(format, frames)?;
    let mut decoder = decoder(&info)?;
    let mut packet = Vec::new();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut output = BufWriter::new(temp.as_file_mut());
        let index = packed.xing_header()?;
        decode(decoder.as_mut(), &info, &index)?;
        decoder.reset();
        output.write_all(&index)?;
        while packed.next_frame(&mut packet) {
            decode(decoder.as_mut(), &info, &packet)?;
            output.write_all(&packet)?;
        }
        output.flush()?;
    }
    temp.as_file().sync_all()?;
    commit_temp(temp, path, replace)
}

/// Export frames that need no repacking: each frame is written byte-exactly
/// and, when the stream's profile is decodable, validated by decoding it
/// before the file commits.
fn export_self_contained(path: &Path, info: Info, frames: Frames, replace: bool) -> Result<bool> {
    let decodable = match info {
        Info::Aac { decodable, .. } => decodable,
        _ => true,
    };
    let mut decoder = decodable.then(|| decoder(&info)).transpose()?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut output = BufWriter::new(temp.as_file_mut());
        for frame in &frames {
            let bytes = frame.as_bytes()?;
            if let Some(decoder) = decoder.as_mut() {
                decode(decoder.as_mut(), &info, bytes)?;
            }
            output.write_all(bytes)?;
        }
        output.flush()?;
    }
    temp.as_file().sync_all()?;
    commit_temp(temp, path, replace)
}

/// Commit the validated temporary file to `path`. With `replace`, an existing
/// destination is replaced, the way an explicit save to a chosen path is.
/// Without it, the destination name is claimed atomically first so that two
/// racing writers, or a foreign file, can never be overwritten; a lost claim
/// reports `false` and the temporary file is simply dropped.
fn commit_temp(temp: tempfile::NamedTempFile, path: &Path, replace: bool) -> Result<bool> {
    if replace {
        temp.persist(path)?;
        return Ok(true);
    }
    match temp.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub fn duration(info: Info, frames: usize) -> Duration {
    Duration::from_secs_f64(frames as f64 * info.samples() as f64 / f64::from(info.rate()))
}

/// Names of the available audio output devices, in the order the system reports
/// them. Duplicate names are collapsed because the menu can only identify a
/// device by name.
pub fn output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for device in devices {
        if let Ok(name) = device.name()
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names
}

/// The playback devices resolved by name so far. Enumerating the system's
/// output devices costs enough per call that scrubbing was unresponsive with
/// a chosen device, so a resolved device is kept for reuse. Only the thread
/// that starts playback touches this map.
fn device_cache() -> &'static Mutex<HashMap<String, cpal::Device>> {
    static CACHE: OnceLock<Mutex<HashMap<String, cpal::Device>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_device(name: &str) -> Result<cpal::Device> {
    let mut cache = device_cache().lock().unwrap();
    if let Some(device) = cache.get(name) {
        return Ok(device.clone());
    }
    let host = cpal::default_host();
    let devices = host
        .output_devices()
        .map_err(|error| format!("Could not list audio output devices: {error}"))?;
    for candidate in devices {
        if candidate.name().ok().as_deref() == Some(name) {
            cache.insert(name.to_string(), candidate.clone());
            return Ok(candidate);
        }
    }
    Err(format!(
        "The playback device \"{name}\" is no longer available. Choose another device from Transport > Playback Device."
    )
    .into())
}

/// Drop a device from the cache so the next open resolves it again. Used
/// when opening a cached device fails: a device object can go stale when a
/// USB interface is unplugged and reconnected, or when its driver restarts.
fn forget_device(name: &str) {
    device_cache().lock().unwrap().remove(name);
}

fn open_output(device: Option<&str>) -> Result<(OutputStream, rodio::OutputStreamHandle)> {
    let Some(name) = device else {
        return OutputStream::try_default()
            .map_err(|error| format!("Could not open the default audio output. {error}").into());
    };
    match try_open_named(name) {
        Ok(opened) => Ok(opened),
        // The cache is not trusted after a failure: resolve the device again
        // and give it one more try before reporting anything. Enumeration is
        // only paid on the failure path.
        Err(_) => {
            forget_device(name);
            try_open_named(name)
        }
    }
}

fn try_open_named(name: &str) -> Result<(OutputStream, rodio::OutputStreamHandle)> {
    let candidate = cached_device(name)?;
    OutputStream::try_from_device(&candidate)
        .map_err(|error| format!("Could not open playback device \"{name}\". {error}").into())
}

/// Where playback packets come from.
enum Packets {
    /// The packed Layer III sequence: the Xing index first, then frames with
    /// their bit reservoirs repaired.
    Mp3 { packed: Packed },
    /// Self-contained frames copied in order.
    Frames { frames: Frames, next: usize },
}

impl Packets {
    /// Write the next packet's full frame bytes into `packet`; `false` at the
    /// end of the timeline. [`decode`] strips the ADTS header itself.
    fn next(&mut self, packet: &mut Vec<u8>) -> Result<bool> {
        match self {
            Packets::Mp3 { packed } => Ok(packed.next_frame(packet)),
            Packets::Frames { frames, next } => {
                let Some(frame) = frames.get(*next) else {
                    return Ok(false);
                };
                *next += 1;
                packet.clear();
                packet.extend_from_slice(frame.as_bytes()?);
                Ok(true)
            }
        }
    }
}

struct Preview {
    info: Info,
    source: Packets,
    decoder: Box<dyn Decoder>,
    packet: Vec<u8>,
    samples: SampleBuffer<f32>,
    next_sample: usize,
    /// Frames still allowed past the first one; `None` plays to the end.
    remaining: Option<u64>,
    error: Arc<Mutex<Option<String>>>,
}

impl Preview {
    fn next_packet(&mut self) -> Result<bool> {
        if self.remaining == Some(0) {
            return Ok(false);
        }
        if !self.source.next(&mut self.packet)? {
            return Ok(false);
        }
        let audio = decode(self.decoder.as_mut(), &self.info, &self.packet)?;
        self.samples.copy_interleaved_ref(audio);
        self.next_sample = 0;
        self.remaining = self.remaining.map(|left| left - 1);
        Ok(true)
    }
}

impl Iterator for Preview {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_sample == self.samples.len() {
            match self.next_packet() {
                Ok(true) => {}
                Ok(false) => return None,
                Err(error) => {
                    *self.error.lock().unwrap() = Some(error.to_string());
                    return None;
                }
            }
        }
        let sample = self.samples.samples()[self.next_sample];
        self.next_sample += 1;
        Some(sample)
    }
}

impl Source for Preview {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.info.channels().into()
    }
    fn sample_rate(&self) -> u32 {
        self.info.rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// A scrub loop: one decoded window that repeats sample-exactly forever at
/// normal speed, so a repeating fragment can be pinned on a position and
/// hunted by ear. Only the transport stops it.
struct Cycle {
    samples: Arc<Vec<f32>>,
    next: usize,
    channels: u16,
    rate: u32,
}

impl Iterator for Cycle {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = self.samples[self.next];
        self.next += 1;
        if self.next == self.samples.len() {
            self.next = 0;
        }
        Some(sample)
    }
}

impl Source for Cycle {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// One open output stream, kept for the lifetime of an editor window so that
/// playback and scrubs start instantly, and endpoints that mute their first
/// milliseconds are not re-initialised on every scrub. The stream is bound to
/// the device it was opened with; a changed selection re-opens it.
pub struct Output {
    _stream: OutputStream,
    handle: rodio::OutputStreamHandle,
    device: Option<String>,
}

impl Output {
    /// Open `device` (None is the system default).
    pub fn open(device: Option<&str>) -> Result<Self> {
        let (stream, handle) = open_output(device)?;
        Ok(Self {
            _stream: stream,
            handle,
            device: device.map(str::to_string),
        })
    }

    /// Whether this stream is already bound to the given device selection.
    pub fn matches(&self, device: Option<&str>) -> bool {
        self.device.as_deref() == device
    }

    /// A sink for playing on this stream; multiple sinks mix.
    pub(crate) fn sink(&self) -> Result<Sink> {
        Sink::try_new(&self.handle).map_err(Into::into)
    }

    /// Start playback at `start`, emitting at most `limit` frames from there
    /// (`None` plays to the end of the timeline). The priming decodes before
    /// the playhead do not count against `limit`.
    pub fn start(
        &self,
        info: Info,
        frames: &Frames,
        start: usize,
        speed: f32,
        limit: Option<usize>,
    ) -> Result<Playback> {
        let mut packet = Vec::new();
        let (source, samples, remaining, decoder) = match info {
            Info::Mp3 { format } => {
                // Prime the decoder before the playhead for its synthesis
                // filter history, walking back to a frame the plan can start
                // on. Only the frames the playback consumes are packed:
                // planning the reservoir over everything remaining made
                // scrubbing toward the file start slower than scrubbing away
                // from it, because the planned window shrank going forward
                // and grew going back.
                let total = frames.len();
                let mut first = start.saturating_sub(12);
                while first > 0 && !can_start_at(format, frames, first) {
                    first -= 1;
                }
                let wanted = limit.map_or(total - start, |left| left + 2);
                let end = (start + wanted).min(total);
                let mut packed = match Packed::new(format, mp3_frames_range(frames, first..end)?) {
                    Err(_) if first > 0 => {
                        first = 0;
                        Packed::new(format, mp3_frames_range(frames, 0..end)?)?
                    }
                    result => result?,
                };
                let mut decoder = decoder(&info)?;
                for _ in first..start {
                    if packed.next_frame(&mut packet) {
                        decode(decoder.as_mut(), &info, &packet)?;
                    }
                }
                if !packed.next_frame(&mut packet) {
                    return Err("The playhead is at the end of the file.".into());
                }
                let audio = decode(decoder.as_mut(), &info, &packet)?;
                let mut samples = SampleBuffer::new(audio.capacity() as u64, *audio.spec());
                samples.copy_interleaved_ref(audio);
                (
                    Packets::Mp3 { packed },
                    samples,
                    limit.map(|left| left.saturating_sub(1) as u64),
                    decoder,
                )
            }
            _ => {
                // Self-contained frames need no priming: playback starts
                // exactly at the playhead.
                let Some(frame) = frames.get(start) else {
                    return Err("The playhead is at the end of the file.".into());
                };
                let mut decoder = decoder(&info)?;
                let audio = decode(decoder.as_mut(), &info, frame.as_bytes()?)?;
                let mut samples = SampleBuffer::new(audio.capacity() as u64, *audio.spec());
                samples.copy_interleaved_ref(audio);
                // Only the frames the playback consumes are handed over: a
                // bounded playback ends at its limit before the window runs
                // out, and continuous playback keeps the whole remainder.
                let end = limit.map_or(frames.len(), |left| (start + left + 2).min(frames.len()));
                (
                    Packets::Frames {
                        frames: frames[start + 1..end].to_vec(),
                        next: 0,
                    },
                    samples,
                    limit.map(|left| left.saturating_sub(1) as u64),
                    decoder,
                )
            }
        };
        let error = Arc::new(Mutex::new(None));
        let sink = self.sink()?;
        sink.set_speed(speed.clamp(0.1, 4.0));
        sink.append(Preview {
            info,
            source,
            decoder,
            packet,
            samples,
            next_sample: 0,
            // The first frame is already decoded above and counts against the
            // limit.
            remaining,
            error: error.clone(),
        });
        Ok(Playback {
            sink,
            start,
            stop_return: start,
            error,
            snippet: limit.is_some(),
            resume: false,
            loop_len: 0,
        })
    }

    /// Start a repeating scrub loop of `length` frames beginning at `start`.
    /// The window is decoded once and then cycles sample-exactly at normal
    /// speed, so a repeating fragment can be hunted by ear; it never ends on
    /// its own — stopping it is the transport's job.
    pub fn start_loop(
        &self,
        info: Info,
        frames: &Frames,
        start: usize,
        length: usize,
    ) -> Result<Playback> {
        let total = frames.len();
        if start >= total || length == 0 {
            return Err("The playhead is at the end of the file.".into());
        }
        let length = length.min(total - start);
        let mut decoder = decoder(&info)?;
        let mut pcm = Vec::new();
        let mut decoded = 0;
        match info {
            Info::Mp3 { format } => {
                // Prime the decoder before the loop start for its synthesis
                // filter history, the way `start` does, and pack only the
                // frames the loop consumes.
                let mut first = start.saturating_sub(12);
                while first > 0 && !can_start_at(format, frames, first) {
                    first -= 1;
                }
                let end = (start + length + 2).min(frames.len());
                let mut packed = match Packed::new(format, mp3_frames_range(frames, first..end)?) {
                    Err(_) if first > 0 => {
                        first = 0;
                        Packed::new(format, mp3_frames_range(frames, 0..end)?)?
                    }
                    result => result?,
                };
                let mut packet = Vec::new();
                for _ in first..start {
                    if packed.next_frame(&mut packet) {
                        decode(decoder.as_mut(), &info, &packet)?;
                    }
                }
                while decoded < length {
                    if !packed.next_frame(&mut packet) {
                        break;
                    }
                    decode_into(decoder.as_mut(), &info, &packet, &mut pcm)?;
                    decoded += 1;
                }
            }
            _ => {
                // Self-contained frames decode one by one, starting exactly
                // at the loop start.
                for frame in &frames[start..start + length] {
                    decode_into(decoder.as_mut(), &info, frame.as_bytes()?, &mut pcm)?;
                    decoded += 1;
                }
            }
        }
        if decoded == 0 {
            return Err("The loop window could not be decoded.".into());
        }
        let error = Arc::new(Mutex::new(None));
        let sink = self.sink()?;
        sink.append(Cycle {
            samples: Arc::new(pcm),
            next: 0,
            channels: info.channels().into(),
            rate: info.rate(),
        });
        Ok(Playback {
            sink,
            start,
            stop_return: start,
            error,
            snippet: true,
            resume: false,
            loop_len: decoded,
        })
    }
}

pub struct Playback {
    pub sink: Sink,
    pub start: usize,
    /// Where the editor's playhead should land when playback is stopped. This
    /// is the playback start unless a pause snapped it to the pause position.
    pub stop_return: usize,
    pub error: Arc<Mutex<Option<String>>>,
    /// True for bounded playback (scrub snippets and selection previews): the
    /// editor keeps the playhead where it is when playback ends instead of
    /// treating it as end of timeline.
    pub snippet: bool,
    /// True for a scrub snippet that interrupted continuous playback: the
    /// editor resumes continuous playback from the playhead when the snippet
    /// ends. Anything else that stops or replaces the playback drops the flag
    /// with it.
    pub resume: bool,
    /// Frames in a repeating scrub loop, or 0 for one-shot playback. The
    /// playhead reports inside the loop instead of running past its end, and
    /// speed changes leave a loop at normal speed.
    pub loop_len: usize,
}

impl Playback {
    pub fn set_speed(&self, speed: f32) {
        if self.loop_len > 0 {
            // A scrub loop always plays at normal speed; the speed keys are
            // for continuous playback.
            return;
        }
        self.sink.set_speed(speed.clamp(0.1, 4.0));
    }

    /// Whether this is a repeating scrub loop rather than one-shot playback.
    pub fn looped(&self) -> bool {
        self.loop_len > 0
    }

    pub fn position(&self, info: Info) -> usize {
        // get_pos counts wall time at the output: rodio's own documentation
        // puts the source position at get_pos times the speed factor, so a
        // 2x playback that reports 5 s is 10 s into the timeline. Without
        // this, every position taken while speed is not 1.00x — reports,
        // markers, and cue points — lands behind the audio that was heard.
        let speed = f64::from(self.sink.speed());
        let frames = self.sink.get_pos().as_secs_f64() * speed * f64::from(info.rate())
            / info.samples() as f64;
        let frames = if self.loop_len > 0 {
            frames % self.loop_len as f64
        } else {
            frames
        };
        self.start + frames.floor() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_no_replace_skips_existing_destinations() {
        let directory = tempfile::tempdir().unwrap();
        let existing = directory.path().join("01 song.aac");
        std::fs::write(&existing, b"original").unwrap();
        let frames: Frames = vec![
            Arc::new(Frame::Data(vec![1, 2, 3].into_boxed_slice())),
            Arc::new(Frame::Data(vec![4, 5].into_boxed_slice())),
        ];
        let info = Info::Aac {
            profile: 1,
            rate: 44_100,
            channels: 2,
            decodable: false,
        };
        assert_eq!(
            export_no_replace(&existing, info, frames.clone()).unwrap(),
            NoReplace::Existed
        );
        assert_eq!(std::fs::read(&existing).unwrap(), b"original");
        let sibling = directory.path().join("02 song.aac");
        assert_eq!(
            export_no_replace(&sibling, info, frames).unwrap(),
            NoReplace::Written(sibling.clone())
        );
        assert_eq!(std::fs::read(&sibling).unwrap(), vec![1, 2, 3, 4, 5]);
    }

    /// Measure a full parse plus validated export without changing the source.
    /// Run in release mode: `cargo test --release -- --ignored export_benchmark`
    /// with `DIRECTCUT_BENCH_FIXTURE` pointing at a local MP3.
    #[test]
    #[ignore = "benchmark: set DIRECTCUT_BENCH_FIXTURE to a local MP3"]
    fn export_benchmark() {
        let input = std::path::PathBuf::from(
            std::env::var_os("DIRECTCUT_BENCH_FIXTURE")
                .expect("set DIRECTCUT_BENCH_FIXTURE to a local MP3"),
        );
        let bytes = std::fs::read(&input).unwrap();
        let parse_start = std::time::Instant::now();
        let scan = crate::mp3::scan(&bytes).unwrap();
        let parse = parse_start.elapsed();
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("benchmark-export.mp3");
        let info = crate::codec::Info::Mp3 {
            format: scan.format,
        };
        let frames: Frames = scan
            .frames
            .into_iter()
            .map(|frame| Arc::new(Frame::Mp3(frame)))
            .collect();
        let export_start = std::time::Instant::now();
        export(&output, info, frames).unwrap();
        let save = export_start.elapsed();
        let written = std::fs::metadata(&output).unwrap().len();
        eprintln!(
            "frames={} input={} bytes output={} bytes parse_ms={} save_ms={}",
            scan.frame_count,
            bytes.len(),
            written,
            parse.as_millis(),
            save.as_millis()
        );
        assert!(written > 0);
    }
}
