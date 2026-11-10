# DirectCut RS

DirectCut RS is a Windows MP3 editor written in Rust. It uses RunGUI's native Windows controls and streams playback through WASAPI. Build with the Rust MSVC toolchain and the Visual Studio C++ Build Tools.

```powershell
cargo build --release --locked
.\target\release\directcut-rs.exe
```

## Features

- Edit MP3 audio at frame boundaries with cut, copy, paste, insert, duplicate, crop, and selection export.
- Navigate CUE titles, update title markers as you edit, and export an updated CUE sheet.
- Scrub audio, preview a selection, choose an output device, and adjust playback speed.
- Analyze damaged archives and inspect recovery details without changing the source.
- Trim an MP3 by CUE titles and save the result as a separate file.
- Use keyboard shortcuts and screen reader announcements for editing and navigation.

## Scope

DirectCutRS **is not, and is not intended to be,** a full service editing suite, audio encoder or transcoder. DirectCut RS is a surgical tool that solves very specific problems with existing MP3 files.

  ## Portable storage

DirectCut RS keeps its own data beside `directcut-rs.exe`: `.freshen` for update recovery and download staging. It does not use AppData, the user profile, or the system temporary directory for this data. Keep the executable in a writable folder; if that folder is read-only, update staging reports an error instead of falling back to another location.

## Editing

Choose **Open MP3** in the launcher, press Ctrl+O in an editor window, or pass file paths on the command line. Each file opens in its own window. Use Alt+Tab to switch windows. The DirectCut audio clipboard is shared across these windows. Use the start and end markers to select a range. Paste replaces a marked range or inserts at the playhead when there is no range. Undo and redo are available separately in each window.

For those familiar with reaper, DirectCut RS edits one timeline. Delete and cut close the gap, and paste with no marked range inserts at the playhead, so ripple editing is always on and no separate toggle is needed. **Insert MP3 at Playhead...** adds another file at the playhead; it requires the same MPEG version, sample rate, and channel count. These operations move whole MP3 frames, and saving rebuilds the frame layout and seek index without using an encoder. Inserted files contribute audio frames only, not tags or artwork.

| Shortcut or command | Action |
| --- | --- |
| Space | Play, resume a pause, or stop and return to the playback start |
| Ctrl+Space | Pause or resume; pausing leaves the playhead at the pause position |
| Ctrl+J | Jump to a time typed as `ss`, `mm:ss`, or `hh:mm:ss`, or to a CUE title typed as `t24` |
| Ctrl+Shift+Left or Ctrl+Shift+Right | Go to the previous or next CUE title |
| Ctrl+Shift+T | Go to a CUE title by number, `t` number, or part of its name |
| Titles > Read Title List | Show every CUE title start and derived length |
| Ctrl+Shift+J | Report the playhead position and current title |
| Ctrl+Shift+Space | Report the time selection |
| Shift+Space | Preview the marked range and stop |
| Shift+= or Shift+- | Increase or decrease playback speed in 0.25x steps, from 0.25x to 4.00x |
| Ctrl+Shift+Backspace | Reset playback speed to 1.00x |
| Transport → Playback Device | Choose the audio output device for this window |
| Transport → Playback Speed | Choose or adjust playback speed from 0.25x to 4.00x |
| Left or Right | Scrub by the current step, 1 frame by default |
| Ctrl+Left or Ctrl+Right | Scrub about one second |
| - or = | Zoom the scrub step in (slower) or out (faster); zooming out doubles the step up to 1 second |
| Ctrl+Home or Ctrl+End, or Shift+Home or Shift+End | Go to the start or end of the file |
| `[` or `]` | Set the start or end marker |
| Ctrl+`[` or Ctrl+`]` | Go to the start or end marker |
| Escape | Clear markers |
| Ctrl+A | Mark the entire timeline |
| Ctrl+X, Ctrl+C, Ctrl+V | Cut, copy, or paste audio |
| Ctrl+D | Duplicate the marked range after itself |
| Insert | Insert another MP3 at the playhead |
| Delete | Delete the marked range and close the gap |
| Edit → Crop to Selection | Keep only the marked range |
| File → Save Selection As... | Export the marked range without changing the timeline |
| Ctrl+Z or Ctrl+Y | Undo or redo |
| Ctrl+S or Ctrl+Shift+S | Save or choose a new output path |
| Ctrl+I | Analyze the source MP3 without modifying it |
| File → Trim Archive... | Trim an MP3 using its CUE titles |
| F6 | Open the timeline status, including position, markers, recovery summary, and last action |
| F1 | Open the keyboard shortcut reference in a read-only text window |

The first save asks for an output path and suggests a separate edited copy. Later saves use that destination. The app validates the new stream before replacing the destination. The opened source is read-only: DirectCut refuses to save over it, and never overwrites its CUE sheet. Closing a window with unsaved edits prompts before discarding them.

Moving the playhead with Left, Right, Ctrl+Left, or Ctrl+Right scrubs: DirectCut stops continuous playback and plays a short snippet of about 0.1 seconds from the new position, so each move is audible while playback is stopped, and moving backward never plays a long stretch of forward audio. The snippet plays at normal speed even when the playback speed is changed. Space still starts continuous playback from the playhead. `-` zooms the scrub step in, down to a single frame, and `=` zooms it out, doubling the step up to one second; `Shift+-` and `Shift+=` still change playback speed. The current step is shown by F6 and announced when it changes.

**Playback Device** lists the system output devices plus **System Default**. The choice applies to the current editor window only. If the selected device disappears, playback reports an error instead of silently falling back to another device. **Playback Speed** and Shift+= / Shift+- change speed from 0.25x to 4.00x in 0.25x steps; Ctrl+Shift+Backspace returns to 1.00x. This is vari-speed: it changes pitch along with speed, not pitch-preserving time compression or expansion. Ctrl+J measures from the start of the timeline, rounds to the nearest MP3 frame, and jumps to the end when the requested time is past the end. Pausing works the way it does in Reaper: pausing snaps the playhead to the paused position, and play or resume continues from there, while Space during playback still returns to the position where playback started, or to the last pause point.

## CUE titles

If an opened MP3 has a matching `.cue` file beside it, DirectCut loads its `TITLE` and `INDEX 01` entries as frame-aligned title markers. If there is no CUE sheet, no titles are loaded and the title commands report that the file has none.

Title starts move with cuts, deletes, pastes, duplicates, inserts, and undo or redo. A title whose audio is removed disappears, and later titles shift earlier. Crop keeps the titles that intersect the selection and rebases them to the new start; if the crop begins inside a title, that title is synthesized at the crop start. CUE sheets store start times only, so a length is derived as the next title start minus this start, with the last title ending at the timeline end. F6 and **Titles > Read Title List** show those derived lengths.

Ctrl+Shift+Left and Ctrl+Shift+Right move to the previous or next title. Both move relative to the title that contains the playhead, so Previous still reaches the earlier title when playback has already moved past a title start; previous before the first title goes to the start of the timeline, and previous at the end of the timeline goes to the last title. Next past the last title goes to the end of the timeline. Ctrl+Shift+T opens **Go to Title**, which accepts a number, `t` and a number, or part of a title name. Ctrl+J also accepts `t` and a number, such as `t24`, in place of a time.

`[` and `]` set the start and end markers at the playhead. Right after a title jump, while the playhead is still inside that title, a marker set with `[` or `]` marks the title start itself instead of the playhead's current frame; move the playhead with the arrow keys to mark a mid-title position. A start marker set at or after the end marker clears that end marker, and the other way around, so a selection is always valid. Marker positions, selection lengths, and delete or cut results are announced through the screen reader.

When the opened MP3 had a CUE sheet, Save and Save As write an updated sidecar CUE next to the exported MP3, and Save Selection As writes one for the selection. A source without a CUE stays MP3-only. DirectCut does not overwrite the opened source or its CUE sheet; choose a different output name. If the destination CUE already exists, DirectCut asks before replacing it. The CUE writer stores `FILE`, `TRACK`, `TITLE`, and `INDEX 01` only, so `PERFORMER`, `REM`, and other fields from a source sheet are not copied to the updated sheet. **Announce Titles While Playing** in the Titles menu speaks each title as playback enters it, so an archive can be proof-listened hands-free.

## Screen reader announcements

Title jumps, marker positions, selection and delete results, position and selection reports, title announcements during playback, and completed saves are announced through Windows UI Automation notifications. This needs no NVDA or JAWS add-on and no bundled speech library. DirectCut asks Windows whether an accessibility client is listening and sends one notification through the window. If no screen reader is running, the call does nothing: there is no SAPI, OneCore, or other text-to-speech fallback. F6 and the timeline status remain the complete accessible record, so speech is never the only way to get information.

The notification path requires Windows 10 version 1709 or later. On older systems the UIA library is loaded at runtime and announcements are simply unavailable; the editor continues to work.

## Archive analysis

Choose **Analyze Source MP3...** in an editor window or press Ctrl+I to inspect the source file without changing it. The result opens in a read-only plain-text report with **Save Report...**. The report gives exact byte ranges for every problem it found, the retained frame count and duration, aggregate skipped-byte and skipped-frame counts, and a SHA-256 of the input. F6 shows a short recovery summary when the imported stream contains a gap.

The report separates observed bytes from estimated missing audio. A byte range is a file position, not a duration. When a gap is joined, retained frames are placed next to each other; nothing is replaced with silence or transcoded audio. The report also states that analysis performs structural parsing only and runs no decoder.

You can analyze a file from a terminal without opening a window:

```powershell
directcut-rs.exe --analyze .\archive.mp3
directcut-rs.exe --analyze .\archive.mp3 --out ".\archive - analysis.txt"
```

Use `--out` to save the report; an existing report is not replaced unless you add `--force`. Without `--out`, the program writes the report to standard output; console output may not be visible in the Windows release build. The command refuses to write a report over the input file. Use a copy of an irreplaceable recording for any hands-on trial, never the only copy.

The current build diagnoses metadata but does not copy it on export. ID3v1, ID3v2, APEv2, artwork, encoder delay, and encoder padding are omitted from saved edits. Analysis reports where those tags were found so the omission is visible. A damaged Xing/Info/VBRI signature can also make the index frame look like a short audio frame; that case is documented in the [archive audit baseline](docs/archive-audit-baseline.md) and is not yet handled. Metadata preservation and a separate validated recovery save are future checkpoints, not features of this build.

## Archive trim

Choose **File → Trim Archive...** to open the CUE-based trimmer.

### Lossless trim from CUE

**Trim Archive...** opens an archived MP3 and its matching `.cue` file, then lets you choose the first and last title to keep. The trim ends at the next CUE entry after the last selected title, or at the end of the file. It uses DirectCut's own MP3 parser and frame-preserving exporter: audio is not decoded or re-encoded, the seek index is rebuilt, and transport bitrate headers may change to fit preserved main data. It writes a new `_trimmed.mp3` next to the source with a matching CUE and, when the session CSV is available, a trimmed title CSV. The source archive is opened read-only and is never overwritten. Existing output names are never replaced; a numeric suffix is added instead.

CUE times are stored at 75 frames per second, so trim boundaries are accurate to the nearest retained MP3 frame, not to an exact sample. The trimmed file has no source metadata, artwork, encoder delay, or padding, for the same reason as ordinary editor exports.

## Portable updates

Extract `directcut-rs.exe` to a writable folder and keep its original filename. DirectCut RS checks GitHub for signed releases in the background each time it starts and opens the update window when a newer release is available. To check manually, choose **About → Check for updates**. Save your work and close all MP3 editor windows and any other DirectCut RS instances before choosing **Install update**.

The download shows byte progress. Verification and installation use a busy indicator while the updater (Freshen) works. A copy of DirectCut RS runs as the update helper, replaces the executable after the app exits, and restarts the app. Its progress window reports completion after the new copy confirms startup. Freshen retains transaction and recovery information under `.freshen` in the application folder. Close the completed helper window before starting another update.

## A note about MP3 boundaries and compatibility

Edits occur at MPEG Layer III frame boundaries. The editor keeps each retained frame's compressed audio and rebuilds the bit reservoir and Xing seek index. It may change transport bitrates to fit the preserved data. Preview decodes for playback, and saving decodes for validation. Saving writes the compressed data directly.

Paste requires matching MPEG version, sample rate, and channel count. MPEG-1, MPEG-2, and MPEG-2.5 Layer III are supported with standard bitrate indices. Free-format MP3 and midstream format changes are unsupported. If a splice needs more reservoir space than the format allows, move the edit boundary or undo the edit.

When an MP3 starts partway through a frame or contains a break, DirectCut RS looks for several consecutive compatible frame headers before it resumes importing. It skips partial bytes, observed damaged frames, and frames whose reservoir history is unavailable. A real change of MPEG version, sample rate, or channel count stops the import at the first stream instead of joining incompatible audio; the unimported remainder is reported. Ctrl+I gives exact byte ranges for each event. F6 gives the short version.

A malformed or truncated ID3v2 tag header no longer blocks audio recovery: only the tag header is skipped, then sync verification continues. A clean removal of whole frames at a frame boundary is not detectable from the remaining file, because the result is still a valid, shorter stream. The importer reports an intact sequence in that case rather than inventing a loss.

Exports contain a fresh seek index. Source tags, artwork, and encoder delay and padding metadata are omitted. A splice can change decoder filter history at the join, so the decoded samples immediately around a cut can differ from the original.

## Tests

Run `cargo test --locked` for the parser, recovery, report, and malformed-input tests. A seeded mutation test requires that damaged or random input never causes a panic, hang, or unbounded scan.

Optional fixture tests generate MP3 files locally from synthesized tones. They are not distributed with the project. Fixture generation requires FFmpeg with its `libmp3lame` encoder and the `lame` command on `PATH`; neither is required to build or run DirectCut RS.

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\generate-fixtures.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\make-corruption-fixtures.ps1

$env:DIRECTCUT_TEST_FIXTURES = 'target\fixtures'
$env:DIRECTCUT_TEST_CORRUPTIONS = 'target\corruptions'
cargo test --locked -- --ignored --skip export_benchmark
```

| Fixture | Format |
| --- | --- |
| `cbr.mp3` | MPEG-1, 44.1 kHz stereo, CBR, ID3v2.4 |
| `vbr.mp3` | MPEG-1, 44.1 kHz stereo, VBR, ID3v2.4 |
| `lsf.mp3` | MPEG-2, 22.05 kHz mono |
| `mpeg25.mp3` | MPEG-2.5, 8 kHz mono |
| `crc.mp3` | MPEG-1, 44.1 kHz stereo, protection bit set |
| `cbr-art.mp3` | MPEG-1, 44.1 kHz stereo, ID3v2.3 with embedded artwork |

The splice test writes `out-*.mp3` files in the fixture folder for independent decoder checks. The corruption script writes deterministic damaged copies and `manifest.txt` to `target\corruptions`. It covers a flipped header, inserted garbage, a short frame, a truncated final frame, missing start bytes, leading and trailing junk, a bogus ID3v2 size, an ID3v1 trailer, a midstream format change, clean whole-frame removal, an empty file, and random garbage. Clean whole-frame removal is a negative control: the result is a valid shorter stream and is reported as intact, because the file alone cannot prove that frames were removed.

An optional benchmark measures parse and validated-export time:

```powershell
$env:DIRECTCUT_BENCH_FIXTURE = 'target\bench\long-30min.mp3'
cargo test --release --locked -- --ignored export_benchmark --nocapture
```

See the [archive audit baseline](docs/archive-audit-baseline.md) for measured results, limitations, and the full corruption matrix.
