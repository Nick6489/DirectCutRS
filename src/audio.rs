use std::{
    io::{BufWriter, Write},
    path::Path,
    sync::{Arc, Mutex},
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
    audio::SampleBuffer,
    codecs::{CODEC_TYPE_MP3, CodecParameters, Decoder, DecoderOptions},
    formats::Packet,
};

use crate::mp3::{Format, Frames, Packed, Result};

fn decoder() -> Result<Box<dyn Decoder>> {
    Ok(symphonia::default::get_codecs().make(
        CodecParameters::new().for_codec(CODEC_TYPE_MP3),
        &DecoderOptions { verify: true },
    )?)
}

fn decode<'a>(
    decoder: &'a mut dyn Decoder,
    format: Format,
    bytes: &[u8],
) -> Result<symphonia::core::audio::AudioBufferRef<'a>> {
    let packet = Packet::new_from_slice(0, 0, format.samples() as u64, bytes);
    let audio = decoder.decode(&packet)?;
    if audio.frames() != format.samples()
        || audio.spec().rate != format.rate
        || audio.spec().channels.count() != usize::from(format.channels)
    {
        return Err("An MP3 frame did not decode to the expected audio format.".into());
    }
    Ok(audio)
}

pub fn export(path: &Path, format: Format, frames: Frames) -> Result<()> {
    let mut packed = Packed::new(format, frames)?;
    let mut decoder = decoder()?;
    let mut packet = Vec::new();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut output = BufWriter::new(temp.as_file_mut());
        let index = packed.xing_header()?;
        decode(decoder.as_mut(), format, &index)?;
        decoder.reset();
        output.write_all(&index)?;
        while packed.next_frame(&mut packet) {
            decode(decoder.as_mut(), format, &packet)?;
            output.write_all(&packet)?;
        }
        output.flush()?;
    }
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    Ok(())
}

pub fn duration(format: Format, frames: usize) -> Duration {
    Duration::from_secs_f64(frames as f64 * format.samples() as f64 / f64::from(format.rate))
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

fn open_output(device: Option<&str>) -> Result<(OutputStream, rodio::OutputStreamHandle)> {
    let Some(name) = device else {
        return OutputStream::try_default()
            .map_err(|error| format!("Could not open the default audio output. {error}").into());
    };
    let host = cpal::default_host();
    let devices = host
        .output_devices()
        .map_err(|error| format!("Could not list audio output devices. {error}"))?;
    for candidate in devices {
        if candidate.name().ok().as_deref() != Some(name) {
            continue;
        }
        return OutputStream::try_from_device(&candidate)
            .map_err(|error| format!("Could not open playback device \"{name}\". {error}").into());
    }
    Err(format!(
        "The playback device \"{name}\" is no longer available. Choose another device from Transport > Playback Device."
    )
    .into())
}

struct Preview {
    format: Format,
    packed: Packed,
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
        if !self.packed.next_frame(&mut self.packet) {
            return Ok(false);
        }
        let audio = decode(self.decoder.as_mut(), self.format, &self.packet)?;
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
        self.format.channels.into()
    }
    fn sample_rate(&self) -> u32 {
        self.format.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

pub struct Playback {
    pub sink: Sink,
    _stream: OutputStream,
    pub start: usize,
    /// Where the editor's playhead should land when playback is stopped. This
    /// is the playback start unless a pause snapped it to the pause position.
    pub stop_return: usize,
    pub error: Arc<Mutex<Option<String>>>,
    /// True for bounded playback (scrub snippets and selection previews): the
    /// editor keeps the playhead where it is when playback ends instead of
    /// treating it as end of timeline.
    pub snippet: bool,
}

impl Playback {
    /// Start playback at `start`, emitting at most `limit` frames from there
    /// (`None` plays to the end of the timeline). The priming decodes before
    /// the playhead do not count against `limit`.
    pub fn start(
        format: Format,
        frames: &[Arc<crate::mp3::Frame>],
        start: usize,
        device: Option<&str>,
        speed: f32,
        limit: Option<usize>,
    ) -> Result<Self> {
        // Prime the decoder before the playhead for its synthesis filter history.
        let mut first = start.saturating_sub(12);
        while first > 0 && !crate::mp3::can_start(format, &frames[first]) {
            first -= 1;
        }
        let mut packed = match Packed::new(format, frames[first..].to_vec()) {
            Err(_) if first > 0 => {
                first = 0;
                Packed::new(format, frames.to_vec())?
            }
            result => result?,
        };
        let mut decoder = decoder()?;
        let mut packet = Vec::new();
        for _ in first..start {
            if packed.next_frame(&mut packet) {
                decode(decoder.as_mut(), format, &packet)?;
            }
        }
        if !packed.next_frame(&mut packet) {
            return Err("The playhead is at the end of the file.".into());
        }
        let audio = decode(decoder.as_mut(), format, &packet)?;
        let mut samples = SampleBuffer::new(audio.capacity() as u64, *audio.spec());
        samples.copy_interleaved_ref(audio);
        let error = Arc::new(Mutex::new(None));
        let (stream, handle) = open_output(device)?;
        let sink = Sink::try_new(&handle)?;
        sink.set_speed(speed.clamp(0.1, 4.0));
        sink.append(Preview {
            format,
            packed,
            decoder,
            packet,
            samples,
            next_sample: 0,
            // The first frame is already decoded above and counts against the
            // limit.
            remaining: limit.map(|left| left.saturating_sub(1) as u64),
            error: error.clone(),
        });
        Ok(Self {
            sink,
            _stream: stream,
            start,
            stop_return: start,
            error,
            snippet: limit.is_some(),
        })
    }

    pub fn set_speed(&self, speed: f32) {
        self.sink.set_speed(speed.clamp(0.1, 4.0));
    }

    pub fn position(&self, format: Format) -> usize {
        let frames =
            self.sink.get_pos().as_secs_f64() * f64::from(format.rate) / format.samples() as f64;
        self.start + frames.floor() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let export_start = std::time::Instant::now();
        export(&output, scan.format, scan.frames).unwrap();
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
