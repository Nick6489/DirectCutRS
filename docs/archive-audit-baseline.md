# DirectCut RS archive audit baseline

Baseline date: 2026-11-10

This document records what the archive-recovery work measured at that baseline.
It is a limitation list, not a promise that every damaged MP3 can be repaired.
Checkpoint 0 (characterization) and Checkpoint 1 (structured diagnostics and
bounded resynchronization) are represented here. Checkpoint 2 (metadata
preservation) and Checkpoint 3 (a separate validated recovery save) have not
been started.

Later commits added CUE title navigation, derived title lengths, UI Automation
announcements, and source-safe sidecar CUE saving. Those features are documented
in `README.md` and the F1 help. Sections below describe the baseline build unless
they explicitly mention the current build. See **Checkpoint status** for the
current state.

## Source and environment

| Item | Value |
| --- | --- |
| Upstream base commit | `8ed73d16fe0d4e1ea065ed574104ca8590249662` (`Prepare DirectCut RS 1.0 with signed portable updates`) |
| Working branch | `feature/archive-recovery-foundation` |
| Operating system | Windows 10 Pro 10.0.19045 |
| Toolchain | rustc 1.99.1 (58cf8f9c3 2026-10-13), cargo 1.99.1, MSVC |
| Independent decoder | FFmpeg 9.0.2-full_build |
| Fixture encoder | LAME 4.0 (via the fixture script) |
| Audio API test environment | WASAPI output was not used by the automated tests |

The Windows release build is the supported build. No Linux build or sanitizer
run is claimed here.

## Test and fixture commands

```powershell
cargo build --release --locked
cargo test --locked

# Optional real-fixture tests. Generate fixtures first.
powershell -ExecutionPolicy Bypass -File .\scripts\generate-fixtures.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\make-corruption-fixtures.ps1

$env:DIRECTCUT_TEST_FIXTURES = 'target\fixtures'
$env:DIRECTCUT_TEST_CORRUPTIONS = 'target\corruptions'
cargo test --locked -- --ignored --skip export_benchmark

# Optional 30-minute parse and validated-export benchmark.
$env:DIRECTCUT_BENCH_FIXTURE = 'target\bench\long-30min.mp3'
cargo test --release --locked -- --ignored export_benchmark --nocapture
```

`generate-fixtures.ps1` synthesizes tones only. It does not read or redistribute
copyrighted recordings or private archives. `make-corruption-fixtures.ps1` writes
byte-for-byte deterministic damaged copies and a manifest; it never modifies the
healthy fixtures.

## What changed in this checkpoint

- `mp3::scan` retains frames and returns exact source offsets plus a structured
  event list. `mp3::inspect` parses without retaining compressed frame data.
- `mp3::parse` keeps the original `(Format, Frames, Recovery)` shape for
  compatibility. `Recovery.skipped_frames` still means only frames dropped for
  missing reservoir history; frames observed but too damaged to import are
  counted on the corresponding event instead.
- Event categories are `metadata`, `leading bytes`, `stray bytes`,
  `truncated frame`, `invalid frame header`, `missing reservoir data`,
  `format change`, and `stream index`. Each event carries a half-open byte range
  in the original file and, only when knowable, an observed frame count.
- Resynchronization now runs after a bad header or garbage region, not only at
  the start of the file. A candidate must be followed by multiple compatible
  frame headers before it is accepted.
- A malformed or truncated ID3v2 tag header no longer prevents audio recovery.
  Only the ten-byte header is skipped, then verified sync search continues.
- A real incompatible format change stops the import with a `FormatChange`
  event. The first segment is retained; the unimported remainder is reported.
- A trailing partial frame is reported explicitly as a truncated frame.
- The editor adds **Analyze Source MP3...** (Ctrl+I), a read-only report window
  with **Save Report...**. F6 includes a short recovery summary.
- The command line adds `--analyze <input> [--out <report>] [--force]`. It
  refuses to write the report over the input and refuses to replace an existing
  report without `--force`.
- Reports contain a SHA-256 of the input, so a later repair can be tied to the
  exact file that was analyzed.

## Healthy fixture results

| Fixture | Format | Observed result |
| --- | --- | --- |
| `cbr.mp3` | MPEG-1, 44.1 kHz stereo, CBR, ID3v2.4 | 231 frames, 6.034 s, intact; tag reported, not copied |
| `vbr.mp3` | MPEG-1, 44.1 kHz stereo, VBR, ID3v2.4 | 231 frames, 6.034 s, intact; tag reported, not copied |
| `lsf.mp3` | MPEG-2, 22.05 kHz mono | 232 frames, 6.060 s, intact |
| `mpeg25.mp3` | MPEG-2.5, 8 kHz mono | 86 frames, 6.192 s, intact |
| `crc.mp3` | MPEG-1, 44.1 kHz stereo, protection bit set | 232 frames, 6.060 s, intact; CRC-protected header accepted |
| `cbr-art.mp3` | MPEG-1, 44.1 kHz stereo, ID3v2.3 with embedded artwork | 231 frames, 6.034 s, intact; 349-byte tag reported, not copied |

All healthy fixtures report `Intact MPEG frame sequence. No audio gaps were
found.` The parser does not transcode, and analysis does not write the input.

## Corruption matrix results

These are the measured outcomes for the generated corruption files. Frame
offsets in the manifest are relative to the MPEG stream after leading tags;
report offsets are absolute file offsets.

| Case | Injected change | Observed result |
| --- | --- | --- |
| `cbr-bad-header.mp3` | MPEG version bits of frame 21 flipped | 228 frames retained; invalid header at 8358..8776; 2 frames dropped for missing reservoir history; resynchronized |
| `cbr-garbage-insert.mp3` | 100 bytes of `0xA5` inserted between frames | 229 frames retained; 100 stray bytes reported; 2 reservoir-dependent frames dropped; both sides retained |
| `cbr-short-frame.mp3` | 50 bytes removed inside frame 26 | 228 frames retained; 368 bytes reported as a short/truncated frame; 2 reservoir-dependent frames dropped; resynchronized |
| `cbr-truncated-final.mp3` | Last 137 bytes removed | 230 frames retained; final 281 bytes reported as an incomplete frame; earlier audio retained |
| `cbr-missing-start.mp3` | First 100 bytes removed | 231 frames retained after resynchronization; 317 leading bytes skipped |
| `cbr-leading-junk.mp3` | 35 junk bytes before audio | 231 frames retained; 35 leading bytes reported |
| `cbr-trailing-junk.mp3` | 23 junk bytes appended | 231 frames retained; 23 trailing bytes reported |
| `cbr-id3-bogus.mp3` | ID3v2 header claims 268,435,455 tag bytes, then 40 zero bytes | 231 frames retained; invalid tag reported; 40 bytes between the header and audio reported as leading bytes |
| `cbr-id3v1.mp3` | 128-byte ID3v1 trailer appended | 231 frames retained; tag reported; zero skipped audio bytes |
| `cbr-format-change.mp3` | MPEG-1 stereo followed by MPEG-2 mono | 231 frames retained from the first stream; 48,691 remaining bytes reported, not imported; streams not joined |
| `cbr-whole-frames-removed.mp3` | Frames 21-40 removed at a clean boundary | 211 frames retained; reports an intact, shorter stream. This edit is not detectable from the file alone |
| `empty.mp3` | Zero bytes | Clear error: `No complete MP3 frame sequence found.` Exit code 1 |
| `garbage.mp3` | 4,096 deterministic pseudo-random bytes | Clear error, no panic or hang. Exit code 1 |

The whole-frame removal case is deliberately included as a negative control. A
clean MP3 frame sequence contains no redundant length or offset record of frames
that are simply absent, so a parser cannot honestly claim to detect that loss
without an external reference.

## Measured performance

Measurements were taken on the release build with the local 30-minute fixture
(`long-30min.mp3`, 28,801,192 bytes, MPEG-1 44.1 kHz stereo).

| Operation | Result |
| --- | --- |
| GUI import of the 30-minute file | Peak working set 76,574,720 bytes (73.0 MiB); window title `long-30min.mp3 - DirectCut RS` |
| CLI read-only analysis (`--analyze`, report to a file) | 176 ms wall time; peak working set 35,184,640 bytes (33.6 MiB) |
| Parse for a full export | 24 ms |
| Validated export to a temporary destination | 1,496 ms; 28,800,887-byte output from 68,908 retained frames |

The export benchmark validates every output packet with Symphonia before the
temporary file is persisted, so the save figure includes decode validation. It
does not measure GUI painting or disk speed on a network share. Multi-hour files
were not loaded; the 30-minute figures are the only measured memory data in this
checkpoint.

## Validation and source immutability

- `cargo test --locked` passes all non-ignored unit tests, including a seeded
  mutation/fuzz loop that requires no panic, hang, or unbounded scan.
- The ignored fixture test splices real streams and writes `out-*.mp3`; those
  outputs decode with FFmpeg (exit 0).
- The ignored corruption test asserts the classifications above.
- SHA-256 before and after analysis matched for `cbr-bad-header.mp3`
  (`29a8516e...e7b9b9`) and `long-30min.mp3`
  (`b107d2e8...f103040`), the same hashes printed in their reports.
- The CLI refuses `--out` equal to the input file, even through a different path.
- Saving an edit is still the existing editor save path. This checkpoint did not
  add a separate `Save Recovered Copy` command or change the save transaction.

## Example report

This is an unedited excerpt from `cbr-bad-header.mp3`:

```text
DirectCut RS archive analysis
Report format 1

File name: cbr-bad-header.mp3
Input size: 96965 bytes
SHA-256: 29a8516e8fe369838971cd49d15f5ae948470dd9f5161ea0786ccddd72e7b9b9
Audio starts at byte 0 after any leading tag data.

Stream: MPEG-1 Layer III, 44100 Hz, stereo (2)
Retained audio: 228 frames, 0:00:05.955
Retained frame bytes: 95294 (headers and side information included)
Skipped: 418 bytes; 2 frame(s) needed unavailable reservoir data.
Status: Recovered with gaps. Audio on each side of a gap is joined; nothing was inserted.
Validation: structural parsing only. No decoder or encoder ran during this analysis.

Events (byte offsets in the original file):
  1. 0..417 (417 bytes) stream index: a Xing, Info, or VBRI index frame was found; a new index is written on save
  2. 8358..8776 (418 bytes) invalid frame header, 1 frame(s): a frame header of another format was skipped; import resumed at a verified frame sequence
  3. 8776..9612 (836 bytes) missing reservoir data, 2 frame(s): an audio frame needed reservoir data that was not available; it was skipped

Metadata: no ID3v1, ID3v2, or APEv2 tag was detected at the file boundaries.

Honest limits:
  - Byte ranges are exact file positions, not time ranges. Skipped audio has no known duration.
  - A gap joins retained frames directly. Nothing was replaced with silence or transcoded audio.
  - Recovered duration counts retained frames only; encoder delay and padding are not known here.
  - A successful parse does not prove that every decoder will interpret the stream identically.
  - This analysis did not modify the input file.
```

The event at 8358 is one observed damaged frame. The 836-byte reservoir event
covers two observed frames that could not be decoded without lost history. The
report does not estimate how many frames the 418-byte invalid-header region
might once have contained.

## Known limitations

### Metadata is diagnosed, not preserved

This build still omits ID3v1, ID3v2, APEv2, artwork, encoder delay, and padding
on export. The analysis reports tag ranges so the omission is visible. Metadata
preservation belongs to Checkpoint 2. APEv2 boundary handling exists in the
parser, but only ID3v1 and ID3v2.3/v2.4 have fixture-level coverage. APEv2 and
unsynchronized or extended-header ID3v2 structures need dedicated fixtures
before they can be called verified.

### A damaged index signature can look like a short audio frame

If a Xing/Info/VBRI signature is damaged, the first frame may not be recognized
as an index. It can then be counted as an audio frame even though it contains
little or no main data. The current report does not distinguish that case. This
needs format research and targeted tests before metadata/index work is claimed
safe.

### Clean frame removal is invisible

Removing whole frames at a frame boundary cannot be detected from the remaining
bytes. The file is a valid, shorter stream. No report can recover the missing
duration without an external reference.

### Format changes stop at the first segment

A real change in MPEG version, sample rate, or channel count is not joined.
Audio after the change is reported as unimported bytes. Separate-segment salvage
is future work.

### Free-format MP3 is unsupported

The parser still requires standard bitrate indices. Free-format Layer III is
rejected rather than guessed.

### Screen-reader behavior was not verified at this baseline

At this baseline, the analyze command, report window, read-only text area, and
Save Report button were keyboard reachable by construction, and F6 was
available. No NVDA, JAWS, or Narrator session was run in this environment, so
this checkpoint did not claim verified screen-reader behavior, focus order, or
speech quality.

The current build raises notifications on a real provider node exposed through
`WM_GETOBJECT` (raising events on the host provider itself is not supported per
Microsoft's docs). NVDA announcements have been verified by ear with the shipped
build on Windows 10 version 22H2. Narrator and JAWS verification with the
installed versions remains open, and automated tests cover only the silent path
and the title/CUE logic.

### Performance is measured for 30 minutes, not multi-hour archives

The import path holds frame metadata in memory and the analysis path reads the
whole file. The 30-minute numbers are measured; multi-hour memory use is not.
Streaming parsing should wait for profiling of a real multi-hour file.

### Analysis is structural

No decoder runs during `--analyze` or Ctrl+I. A structurally valid stream can
still fail in a particular decoder. The existing save path performs Symphonia
decode validation; analysis deliberately does not, so that it stays fast and
read-only.

### Save workflow was unchanged at this baseline

At this baseline there was no separate recovery preflight, no `Save Recovered
Copy`, and no sidecar report written automatically; the ordinary editor save
applied. Checkpoint 3 remains the place for a distinct, validated recovery
export.

The current build keeps that recovery export unfinished but hardens ordinary
saving: Save and Save As refuse to overwrite the opened source or its CUE sheet,
and Save, Save As, and Save Selection As write an updated sidecar CUE when the
opened source had one. A source without a CUE stays MP3-only. Metadata is still
not preserved.

## Checkpoint status

| Checkpoint | Status |
| --- | --- |
| 0 - Baseline characterization | Implemented and measured; screen-reader and multi-hour verification remain open |
| 1 - Structured diagnostics and safer detection | Implemented for the corruption matrix above; damaged-index and metadata edge cases remain open |
| 2 - Metadata and gapless preservation | Not started |
| 3 - Validated recovery and safe save | Not started; ordinary saves now refuse to overwrite the source |
| 4 - Access and archive editing refinements | Partially implemented: CUE titles, derived lengths, title navigation, UIA announcements (verified by ear with NVDA); Narrator and JAWS verification, selection nudges, and preview looping remain open |

The honest summary is: damaged audio can now be analyzed without changing the
source, bounded resynchronization works on the injected cases, and reports state
what was observed and what was skipped. The baseline build could not preserve
metadata, could not detect clean frame removal, and had not been verified with a
screen reader or a multi-hour recording. Later work added CUE titles and UIA
announcements; announcements are now verified by ear with NVDA, and Narrator and
JAWS verification remains open.
