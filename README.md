# DirectCut RS

DirectCut RS is a Windows MP3 editor written in Rust. It uses RunGUI's native Windows controls and streams playback through WASAPI. Build with the Rust MSVC toolchain and the Visual Studio C++ Build Tools.

```powershell
cargo build --release --locked
.\target\release\directcut-rs.exe
```

## Editing

Choose **Open MP3** in the launcher, press Ctrl+O in an editor window, or pass file paths on the command line. Each file opens in its own window. Use Alt+Tab to switch windows. The DirectCut audio clipboard is shared across these windows. Use the start and end markers to select a range. Paste replaces a marked range or inserts at the playhead when there is no range. Undo and redo are available separately in each window.

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

## Portable updates

Extract `directcut-rs.exe` to a writable folder and keep its original filename. DirectCut RS checks GitHub for signed releases in the background each time it starts and opens the update window when a newer release is available. To check manually, choose **About → Check for updates**. Save your work and close all MP3 editor windows and any other DirectCut RS instances before choosing **Install update**.

The download shows byte progress. Verification and installation use a busy indicator while Freshen works. A copy of DirectCut RS runs as the update helper, replaces the executable after the app exits, and restarts the app. Its progress window reports completion after the new copy confirms startup. Freshen retains transaction and recovery information under `.freshen` in the application folder. Close the completed helper window before starting another update.

### Package a release

Use `freshen-release.exe` built from the pinned Freshen revision in `Cargo.toml`. With PowerShell 7.4 or later, run:

```powershell
.\scripts\package-release.ps1 -FreshenRelease 'C:\path\to\freshen-release.exe' -SigningKey 'C:\private\freshen-ed25519.key'
```

The script stages a signed release under `target/dist`. The ZIP contains only `directcut-rs.exe`. Attach the ZIP, `freshen-manifest.json`, and `freshen-manifest.json.sig` as separate GitHub assets for the `v<VERSION>` tag, where `VERSION` matches `Cargo.toml`. Keep the signing key outside the checkout and release assets.

### Rehearse an update

Run `.\scripts\new-update-demo.ps1 -FreshenRelease 'C:\path\to\freshen-release.exe'` in PowerShell 7.4 or later. Open the printed executable and wait for the automatic update prompt. Choose **Install update**. The demo reads a signed local ZIP slowly enough to follow download progress. The real Freshen helper replaces the demo executable and restarts it as version 1.0. The demo has its own signing key and folder. Run the script again for a fresh copy of version 0.9.

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
