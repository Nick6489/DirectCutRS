# DirectCut RS

DirectCut RS is a Windows MP3 editor written in Rust. It uses RunGUI's native Windows controls and streams playback through WASAPI. Build with the Rust MSVC toolchain and the Visual Studio C++ Build Tools.

```powershell
cargo build --release --locked
.\target\release\directcut-rs.exe
```

## Editing

Open one or more MP3 files with Ctrl+O, or pass their paths on the command line. Each file opens in its own window. Use Alt+Tab to switch windows. The DirectCut audio clipboard is shared across these windows. Use the start and end markers to select a range. Paste replaces a marked range or inserts at the playhead when there is no range. Undo and redo are available separately in each window.

| Shortcut | Action |
| --- | --- |
| Space | Play or stop and return to the playback start |
| Ctrl+Space | Pause or resume |
| Left or Right | Move one MP3 frame |
| Ctrl+Left or Ctrl+Right | Move about one second |
| Shift+Home or Shift+End | Go to the start or end of the file |
| `[` or `]` | Set the start or end marker |
| Ctrl+`[` or Ctrl+`]` | Go to the start or end marker |
| Escape | Clear markers |
| Ctrl+A | Mark the entire timeline |
| Ctrl+X, Ctrl+C, Ctrl+V | Cut, copy, or paste audio |
| Delete | Delete the marked range |
| Ctrl+Z or Ctrl+Y | Undo or redo |
| Ctrl+S or Ctrl+Shift+S | Save or choose a new output path |
| F6 | Open the timeline status, including position, markers, and last action |
| F1 | Open the keyboard shortcut reference |

The first save asks for an output path and suggests a separate edited copy. Later saves use that destination. The app validates the new stream before replacing the destination. Closing a window with unsaved edits prompts before discarding them.

## MP3 boundaries and compatibility

Edits occur at MPEG Layer III frame boundaries. The editor keeps each retained frame's compressed audio and rebuilds the bit reservoir and Xing seek index. It may change transport bitrates to fit the preserved data. Preview decodes for playback, and saving decodes for validation. Saving writes the compressed data directly.

Paste requires matching MPEG version, sample rate, and channel count. MPEG-1, MPEG-2, and MPEG-2.5 Layer III are supported with standard bitrate indices. Free-format MP3 and midstream format changes are unsupported. If a splice needs more reservoir space than the format allows, move the edit boundary or undo the edit.

When an MP3 starts partway through a frame or contains a break, DirectCut RS looks for consecutive frame headers to resume importing. It skips partial bytes and frames with incomplete reservoir data. F6 reports what import recovery skipped. The recovered audio is joined at the break, and saving still requires every output frame to decode successfully.

Exports contain a fresh seek index. Source tags, artwork, and encoder delay and padding metadata are omitted. A splice can change decoder filter history at the join, so the decoded samples immediately around a cut can differ from the original.

## Tests

Run `cargo test --locked` for the reservoir and malformed-input tests. An optional splice test uses locally generated MP3 fixtures and verifies that an export decodes and preserves every retained frame's main data and audio side information. Set `DIRECTCUT_TEST_FIXTURES` to a folder containing these fixtures and run `cargo test --locked splice_real_streams -- --ignored`.

| Fixture | Format |
| --- | --- |
| `cbr.mp3` | MPEG-1, 44.1 kHz stereo, CBR |
| `vbr.mp3` | MPEG-1, 44.1 kHz stereo, VBR |
| `lsf.mp3` | MPEG-2, 22.05 kHz mono |
| `mpeg25.mp3` | MPEG-2.5, 8 kHz mono |
| `crc.mp3` | MPEG-1, 44.1 kHz stereo, with CRCs |

Use at least five seconds of audio for each fixture. The test writes `out-*.mp3` files in the fixture folder for independent decoder checks.
