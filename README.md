# DirectCut RS

DirectCut RS is a Windows MP3, MP2, and ADTS AAC editor written in Rust. It uses RunGUI's native Windows controls and streams playback through WASAPI. Build with the Rust MSVC toolchain and the Visual Studio C++ Build Tools.

```powershell
cargo build --release --locked
.\target\release\directcut-rs.exe
```

## Portable storage

DirectCut RS keeps its own data beside `directcut-rs.exe`: `settings.toml` for the app-wide choices (playback device, scrub zoom, playback speed), and `.freshen` for update recovery and download staging. It does not use AppData, the user profile, or the system temporary directory for this data. Keep the executable in a writable folder; if that folder is read-only, saving preferences or staging an update reports an error instead of falling back to another location.

## Editing

Choose **Open Audio** in the launcher, press Ctrl+O in an editor window, or pass file paths on the command line. Each file opens in its own window. Use Alt+Tab to switch windows. The DirectCut audio clipboard is shared across these windows. Use the start and end markers to select a range. Paste replaces a marked range or inserts at the playhead when there is no range; replacing a marked range longer than about ten seconds asks first unless you chose **Don’t ask again**. Undo and redo are available separately in each window.

DirectCut RS edits one timeline. Delete and cut close the gap, and paste with no marked range inserts at the playhead, so ripple editing is always on and no separate toggle is needed. **Insert Audio at Playhead...** adds another file at the playhead; it requires the same format, sample rate, and channel count, and it asks before combining MP3s of different bitrates (neither part is re-encoded; the exported file may use VBR). These operations move whole audio frames, and saving rebuilds the frame layout and seek index without using an encoder. Inserted files contribute audio frames only, not tags or artwork.

When a file is being inserted, DirectCut says so ("Reading part 2 for insertion..."), and the finished insert or paste speaks one consolidated result: how many frames and how much audio were added, from which file and at what position, and how long the timeline is now. If the timeline already has cue points, the incoming audio comes from a different source, and none sits exactly where it begins, DirectCut asks whether to add one there — named `$file`, which always shows the file the audio at it came from — so a later split or save can respect the file boundaries. When the file has no cue points at all — dialog takes, for instance — nothing is ever asked.

| Shortcut or command | Action |
| --- | --- |
| Space | Play, resume a pause, or stop and return to the playback start |
| Ctrl+Space | Pause or resume; pausing leaves the playhead at the pause position |
| Ctrl+J | Jump to a time typed as `ss`, `mm:ss`, or `hh:mm:ss`, or to a CUE title typed as `t24`; the dialog starts at the playhead — its own time, or the `t` number when the playhead is at a title boundary |
| Ctrl+Shift+Left or Ctrl+Shift+Right | Go to the previous or next CUE title |
| Ctrl+Shift+T | Go to a CUE title by number, `t` number, or part of its name |
| Ctrl+Shift+N or S | Add a cue point at the playhead (Titles > Cue Point) |
| Ctrl+Shift+R | Rename the cue point that contains the playhead |
| Ctrl+Shift+D | Delete the cue point that contains the playhead |
| Ctrl+Shift+M | Move the nearest cue point to the playhead |
| Ctrl+Shift+B or Ctrl+Shift+F | Shift every cue point back or forward one second |
| Titles > Read Title List | Show every CUE title start and derived length |
| Titles > Split Audio at Cue Points... | Write each CUE title's audio as its own numbered file in a chosen folder |
| Ctrl+Shift+J | Report the playhead position and current title |
| Ctrl+Shift+Space | Report the time selection |
| Shift+Space | Preview the marked range and stop |
| Shift+= or Shift+- | Increase or decrease playback speed in 0.25x steps, from 0.25x to 4.00x |
| Ctrl+Shift+Backspace | Reset playback speed to 1.00x |
| Transport > Continue Playback After Scrubbing | Whether playback continues from the playhead after a scrub ends (checked by default) |
| Transport → Playback Device | Choose the audio output device for this window |
| Transport → Playback Speed | Choose a speed from 0.25x to 4.00x |
| Left or Right | Scrub by the current step, 1 frame by default |
| Ctrl+Left or Ctrl+Right | Scrub about one second |
| - or = | Zoom the scrub step in (slower) or out (faster); zooming out doubles the step up to 1 second |
| / | Toggle loop segment scrub: a scrub move repeats a window one scrub step long, starting at the playhead |
| Ctrl+Home or Ctrl+End | Go to the start or end of the file |
| `[` or `]` | Set the start or end marker |
| Ctrl+`[` or Ctrl+`]` | Move the start of the selection backward or forward one scrub step |
| Alt+`[` or Alt+`]` | Move the end of the selection backward or forward one scrub step |
| Alt+Home or Alt+End | Go to the start or end marker |
| Home or End | Go to the start or end of the selection; without a complete selection, a spoken message says so instead of moving |
| Escape (or Edit > Clear Time Selection) | Clear both time-selection markers; leaves the playhead and cue points alone |
| Ctrl+A | Mark the entire timeline |
| Ctrl+X, Ctrl+C, Ctrl+V | Cut, copy, or paste audio; paste confirms before replacing a marked range longer than about ten seconds |
| Ctrl+D | Duplicate the marked range after itself |
| Insert | Insert another MP3 at the playhead |
| Delete | Delete the marked range and close the gap |
| Edit → Crop to Selection | Keep only the marked range |
| File → Save Selection As... | Export the marked range without changing the timeline |
| Ctrl+Z or Ctrl+Y | Undo or redo |
| Ctrl+S or Ctrl+Shift+S | Save or choose a new output path |
| Ctrl+I | Analyze the source MP3 without modifying it |
| Ctrl+V in the opening window | Open supported audio files copied in File Explorer |
| F6 | Open the timeline status, including position, markers, the file's average bitrate, recovery summary, and last action |
| F1 | Open the keyboard shortcut reference |
| F1 or Help > Keyboard Shortcuts | Show every shortcut as a reviewable plain-text document with Save Report |
| File → Paste Audio from Clipboard (Ctrl+V, launcher) | Open the MP3, MP2, or AAC files copied in a file manager |

The first save asks for an output path and suggests a separate edited copy. Later saves use that destination. The app validates the new stream before replacing the destination. The opened source is read-only: DirectCut refuses to save over it, and never overwrites its CUE sheet. Closing a window with unsaved edits prompts before discarding them.

Moving the playhead with Left, Right, Ctrl+Left, or Ctrl+Right scrubs: DirectCut plays a short snippet of about 0.1 seconds that begins exactly at the new position and runs a little past it, so what you hear always starts at the playhead and extends a bit beyond it, never ends just before it. The snippet plays at normal speed even when the playback speed is changed. If continuous playback is running when you scrub, it resumes from the playhead once the snippet ends; when the transport is stopped or paused, only the snippet sounds and the playhead stays put. Transport > Continue Playback After Scrubbing, checked by default, turns that resume off. Space still starts continuous playback from the playhead. `-` zooms the scrub step in, down to a single frame, and `=` zooms it out, doubling the step up to one second; `Shift+-` and `Shift+=` still change playback speed. The current step is shown by F6 and announced when it changes.

Slash (`/`) toggles **Loop Segment Scrub**, the way Reaper toggles segment scrub. While it is on, a scrub move repeats a window exactly as long as the current scrub step, starting at the playhead, over and over until it is stopped — so finding an exact start is a matter of nudging until the wanted sound sits at the beginning of the repeating window. The zoom keys set that window's length, a loop plays at normal speed whatever the playback speed is, and Space stops the loop with the playhead left at its start, so a second Space press plays on from the found point. Turning the toggle on while the transport is stopped starts a loop at once, and turning it off stops a sounding loop and leaves the playhead at the loop's start; the toggle is remembered between sessions, and F6 shows the state as Scrub loop.

**Playback Device** lists the system output devices plus **System Default**. The choice applies to the current editor window only. If the selected device disappears, playback reports an error instead of silently falling back to another device. **Playback Speed** and Shift+= / Shift+- change speed from 0.25x to 4.00x in 0.25x steps; Ctrl+Shift+Backspace returns to 1.00x. This is vari-speed: it changes pitch along with speed, not pitch-preserving time compression or expansion. Ctrl+J measures from the start of the timeline, rounds to the nearest MP3 frame, and jumps to the end when the requested time is past the end. Pausing works the way it does in Reaper: pausing snaps the playhead to the paused position, and play or resume continues from there, while Space during playback still returns to the position where playback started, or to the last pause point. Position reports, markers, and cue points are correct at any playback speed; the playhead tracks the vari-speed factor the way the audio plays.

A timeline strip above the status shows the whole timeline at a glance: the playhead, the start and end markers, and a tick at every CUE title. It is a drawn graphic that repaints as you scrub — it never takes keyboard focus, so it says nothing to a screen reader; F6 and the position reports remain how the strip's contents are read. While anything is playing, the strip's playhead and the status bar's position line follow the audio, so the current time is always visible without pressing anything.

## Combining files

Two workflows use the same tools, and they want different things from the file boundaries.

**Combining dialog takes** — replacing a flubbed line, assembling alternate takes — is a matter of marking what to replace and pasting the better take over it. These files have no cue points, so nothing is ever added at the boundaries: paste over a marked range (a range longer than about ten seconds is confirmed first, because a marked range that long is usually what an earlier paste left behind rather than something being aimed at), or insert at the playhead. Each paste and insert answers with one spoken result naming what was added, where it landed, and the new length, so a paste that went to the wrong place is heard at once and undone.

**Combining segments that already have a cue sheet** — show parts, station segments — wants the sheet to respect where each file begins. With cue points already in the timeline, adding audio from another source at a position without an existing cue asks: *Add a cue point at the file boundary?* Saying yes adds one named `$file`, which always shows the file the audio at it came from; **Titles > Split Audio at Cue Points...** then writes one file per segment under those names, and a save writes the boundaries into the CUE sheet.

Both ways stay lossless: whole frames move, nothing is decoded or re-encoded, and saving validates the result before it replaces the destination.

One thing to know when the files being combined were encoded at different bitrates: DirectCut asks before combining them. The finished file is still lossless in the sense that neither part is re-encoded — each part keeps its compressed audio and its own rate — and most players handle a mixed-rate file, but some hardware players and broadcast tools expect one constant rate. The ask names both rates (about 128 kbps and about 320 kbps, say), so the decision is an informed one; saying no cancels just that combine. F6 shows the file's average bitrate at any time.

## CUE titles

If an opened MP3 has a matching `.cue` file beside it, DirectCut loads its `TITLE` and `INDEX 01` entries as frame-aligned title markers. Titles can also be created here: **Titles > Cue Point > New Cue Point at Playhead** (Ctrl+Shift+N or S) marks the playhead, whether or not the file arrived with a CUE sheet, and a saved timeline then writes a CUE sheet beside the export. The add opens a name prompt prefilled with the cue point that contains the playhead, so a series can be accepted as part 1, then part 2 and part 3; a blank name becomes `Untitled`.

A cue point name may carry **`$file`**, which stands for the name (without its extension) of the file the audio at the cue point came from. The token stays in the stored name and is replaced everywhere the name is shown, written, or used — the status line, the title list, the CUE sheet, and the file names a split writes — so a cue point inside audio pasted in from another file names that file. The token follows the audio: copy a take into a different window and the same name now names that window's file, and moving or deleting audio around a `$file` cue point keeps its name pointing at whatever audio actually sits under it.

After a paste or insert, when the timeline already has cue points and none sits exactly where the pasted audio begins, DirectCut asks whether to add a cue point at that file boundary. Saying yes adds one named `$file` (undoable, like every cue point edit), so splitting or saving marks where each pasted file begins. Saying no adds nothing, and when the file has no cue points at all — dialog takes, for instance — nothing is ever asked.

**Titles > Cue Point** edits the markers themselves: **Rename Cue Point** (Ctrl+Shift+R) renames the cue point that contains the playhead, **Delete Cue Point** (Ctrl+Shift+D) removes it, **Move Nearest Cue Point to Playhead** (Ctrl+Shift+M) moves the nearest marker onto the playhead, and **Shift Cue Points Back/Forward One Second** (Ctrl+Shift+B and Ctrl+Shift+F) move every marker by one second, repeatable, for aligning titles that a station reports a fixed amount of time before or after the audio changes. If cues would meet at the timeline boundary, DirectCut asks before merging them. A merge keeps the later cue. Every cue point edit is undoable and marks the file as edited.

Title starts move with cuts, deletes, pastes, duplicates, inserts, and undo or redo. A title whose audio is removed disappears, and later titles shift earlier. Crop keeps the titles that intersect the selection and rebases them to the new start; if the crop begins inside a title, that title is synthesized at the crop start. CUE sheets store start times only, so a length is derived as the next title start minus this start, with the last title ending at the timeline end. F6 and **Titles > Read Title List** show those derived lengths.

Ctrl+Shift+Left and Ctrl+Shift+Right move to the previous or next title. Both move relative to the title that contains the playhead, so Previous still reaches the earlier title when playback has already moved past a title start, and previous at the end of the timeline goes to the last title. Navigation never yanks the playhead to a boundary as a side effect: with no next title the playhead stays where it was and DirectCut says so, and the file end is reached with Ctrl+End when wanted. Ctrl+Shift+T opens **Go to Title**, which accepts a number, `t` and a number, or part of a title name. Ctrl+J also accepts `t` and a number, such as `t24`, in place of a time.

**Titles > Split Audio at Cue Points...** writes every title's audio as its own file in a chosen folder, named by track number and the cue point's shown name, so a name carrying `$file` is written as the file its audio starts from (`01 Intro - show.mp3`, `02 Promo - promo.mp3`, ...). Each file runs from its title to the next, and the last runs to the end of the timeline; DirectCut asks before splitting if audio before the first cue would be left out. An existing output name is never replaced — a numbered sibling is written instead — and the split is lossless: whole frames are copied to the new files without decoding or re-encoding.

`[` and `]` set the start and end markers at the playhead. Ctrl+[ and Ctrl+] move the start of the selection backward or forward one scrub step, and Alt+[ and Alt+] move its end the same way, mirroring Reaper's time-selection trims; the playhead follows the moved edge and scrubs there, so each trim is audible by ear. Alt+Home and Alt+End go to the start or end marker. Right after a title jump, while the playhead is still inside that title, a marker set with `[` or `]` marks the title start itself instead of the playhead's current frame; move the playhead with the arrow keys to mark a mid-title position. A start marker set or moved to or past the end marker clears that end marker, and the other way around, so a selection is always valid. Marker positions, selection lengths, and delete or cut results are announced through the screen reader. If you want to clear the time selection, just press Escape, or go to Edit > Clear Time Selection. You will hear "Time selection cleared," and the status and timeline will show that the markers are gone; with nothing marked, it says so instead — "No time selection to clear." — and changes nothing. This does not delete any audio or cue points, and it leaves the playhead where it is. Escape does nothing in the opening window when no audio is loaded.

Save and Save As write an updated sidecar CUE next to the exported MP3 whenever the timeline has titles, whether they came from a loaded CUE sheet or were added with the cue point commands, and Save Selection As writes one for the selection. DirectCut does not overwrite the opened source or its CUE sheet; choose a different output name. The CUE writer stores `FILE`, `TRACK`, `TITLE`, and `INDEX 01` only, so `PERFORMER`, `REM`, and other fields from a source sheet are not copied to the updated sheet. **Announce Titles While Playing** in the Titles menu speaks each title as playback enters it, so an archive can be proof-listened hands-free.

## Screen reader announcements

Title jumps, marker positions, selection and delete results, position and selection reports, title announcements during playback, and completed saves are announced through Windows UI Automation notifications. This needs no NVDA or JAWS add-on and no bundled speech library. DirectCut asks Windows whether an accessibility client is listening and sends one notification through the window. If no screen reader is running, the call does nothing: there is no SAPI, OneCore, or other text-to-speech fallback. F6 and the timeline status remain the complete accessible record, so speech is never the only way to get information.

A finished insert or paste speaks one consolidated result — what was added, from which file, where it landed, and the new length — instead of leaving the outcome only in the status line. And when a dialog closes (a name prompt, a warning, a file picker, a report window), keyboard focus returns to where the window had it: the control that held focus before the dialog is asked for again, immediately and again after Windows finishes handing activation back, so keys keep working and the screen reader does not land inside a text field that changes on every scrub.

The notification path requires Windows 10 version 1709 or later. On older systems the UIA library is loaded at runtime and announcements are simply unavailable; the editor continues to work. NVDA announcements have been verified by ear with the shipped build on Windows 10 version 22H2. Narrator and JAWS support should be verified with the installed versions.

## Archive analysis

Choose **Analyze Source MP3...** in an editor window or press Ctrl+I to inspect the source file without changing it. The result opens in a read-only plain-text report with **Save Report...**. The report gives exact byte ranges for every problem it found, the retained frame count and duration, aggregate skipped-byte and skipped-frame counts, and a SHA-256 of the input. F6 shows a short recovery summary when the imported stream contains a gap.

ID3v2 and ID3v1 tags are described in the report: title, artist, album, year, artwork presence, and ReplayGain values when present, with a count of unrecognized frames and a note when the tag could not be read to the end. Tags are described but never copied or modified; saving writes audio only.

The report separates observed bytes from estimated missing audio. A byte range is a file position, not a duration. When a gap is joined, retained frames are placed next to each other; nothing is replaced with silence or transcoded audio. The report also states that analysis performs structural parsing only and runs no decoder.

You can analyze a file from a terminal without opening a window:

```powershell
.\target\release\directcut-rs.exe --analyze .\archive.mp3
.\target\release\directcut-rs.exe --analyze .\archive.mp3 --out ".\archive - analysis.txt"
```

Without `--out`, the report is printed to the console. With `--out`, an existing report is not replaced unless you add `--force`. The command refuses to write a report over the input file. Use a copy of an irreplaceable recording for any hands-on trial, never the only copy.

The current build diagnoses metadata but does not copy it on export. ID3v1, ID3v2, APEv2, artwork, encoder delay, and encoder padding are omitted from saved edits. Analysis reports where those tags were found and what they contain so the omission is visible. A damaged Xing/Info/VBRI signature can also make the index frame look like a short audio frame; that case is documented in `docs/archive-audit-baseline.md` and is not yet handled. Metadata preservation and a separate validated recovery save are future checkpoints, not features of this build.

## Trim an existing archive

**Trim Archive...** opens an archived MP3 and its matching `.cue` file, then lets you choose the first and last title to keep. The trim ends at the next CUE entry after the last selected title, or at the end of the file. It uses DirectCut's own MP3 parser and frame-preserving exporter: audio is not decoded or re-encoded, the seek index is rebuilt, and transport bitrate headers may change to fit preserved main data. It writes a new `_trimmed.mp3` next to the source with a matching CUE and, when the session CSV is available, a trimmed title CSV. The source archive is opened read-only and is never overwritten. Existing output names are never replaced; a numeric suffix is added instead.

CUE times are stored at 75 frames per second and titles arrive with ICY metadata, so trim boundaries are accurate to the nearest retained MP3 frame, not to an exact sample. The trimmed file has no source metadata, artwork, encoder delay, or padding, for the same reason as ordinary editor exports.

## Portable updates

Extract `directcut-rs.exe` to a writable folder and keep its original filename. DirectCut RS checks GitHub for signed releases in the background each time it starts and opens the update window when a newer release is available. To check manually, choose **About → Check for updates**. Save your work and close all audio editor windows and any other DirectCut RS instances before choosing **Install update**.

The download shows byte progress. Verification and installation use a busy indicator while Freshen works. A copy of DirectCut RS runs as the update helper, replaces the executable after the app exits, and restarts the app. Its progress window reports completion after the new copy confirms startup. Freshen retains transaction and recovery information under `.freshen` in the application folder. Close the completed helper window before starting another update.

### Package a release

Use `freshen-release.exe` built from the pinned Freshen revision in `Cargo.toml`. With PowerShell 7.4 or later, run:

```powershell
.\scripts\package-release.ps1 -FreshenRelease 'C:\path\to\freshen-release.exe' -SigningKey 'C:\private\freshen-ed25519.key'
```

The script stages a signed release under `target/dist`. The ZIP contains only `directcut-rs.exe`. Attach the ZIP, `freshen-manifest.json`, and `freshen-manifest.json.sig` as separate GitHub assets for the `v<VERSION>` tag, where `VERSION` matches `Cargo.toml`. Keep the signing key outside the checkout and release assets.

### Rehearse an update

Run `.\scripts\new-update-demo.ps1 -FreshenRelease 'C:\path\to\freshen-release.exe'` in PowerShell 7.4 or later. Open the printed executable and wait for the automatic update prompt. Choose **Install update**. The demo reads a signed local ZIP slowly enough to follow download progress. The real Freshen helper replaces the demo executable and restarts it as version 1.0. The demo has its own signing key and folder. Run the script again for a fresh copy of version 0.9.

## Lossless capabilities

Everything DirectCut RS does is measured against one question: is the result provably lossless? The answer differs by format, and the app says which answer applies to the loaded file in F6, in the Help dialog, and in every analysis report.

Cut, delete, crop, paste, duplicate, and insert move whole audio frames. The retained audio is not re-encoded. The editor rebuilds the frame layout and indexes as needed; the exported file is not necessarily byte-for-byte identical to the source. This is lossless for every framed format, and it is what the editor does today for MP3, MP2, and ADTS AAC. MP3 additionally repacks the bit reservoir across frame boundaries; MP2 and AAC frames are self-contained and need no repacking.

Fades, gain, and normalization change decoded samples. They are only bit-exact where decode and encode round-trip losslessly (FLAC, ALAC, and PCM/WAV). For MP3, MP2, and AAC they are refused rather than approximated: rewriting frame gains quantizes the change to the codec's gain steps, and decode-then-re-encode loses a coding generation. Neither is offered as if it were lossless.

MP2 and ADTS AAC imports are deliberately strict: there is no resynchronisation after a damaged frame, because unlike Layer III these formats have no reservoir state to lose, and refusing to guess keeps the timeline honest. An AAC stream that this build cannot decode (anything beyond AAC-LC in mono or stereo, such as HE-AAC/SBR) still opens and edits, and saves with structural validation only; F6 states this on the file itself. Analysis reports remain MP3-specific, and Ctrl+I says so for other formats.

## MP3, MP2, and AAC boundaries and compatibility

MP3 edits occur at MPEG Layer III frame boundaries. The editor keeps each retained frame's compressed audio and rebuilds the bit reservoir and Xing seek index. It may change transport bitrates to fit the preserved data. Preview decodes for playback, and saving decodes for validation. Saving writes the compressed data directly.

Paste requires matching format, sample rate, and channel count. MPEG-1, MPEG-2, and MPEG-2.5 Layer III are supported with standard bitrate indices. Free-format MP3 and midstream format changes are unsupported. If a splice needs more reservoir space than the format allows, move the edit boundary or undo the edit. Files encoded at different bitrates can be combined — each frame keeps its own transport rate, and saving may raise a frame's transport bitrate only where its data needs more room — but the combine asks first when the two average bitrates differ.

When an MP3 starts partway through a frame or contains a break, DirectCut RS looks for several consecutive compatible frame headers before it resumes importing. It skips partial bytes, observed damaged frames, and frames whose reservoir history is unavailable. A real change of MPEG version, sample rate, or channel count stops the import at the first stream instead of joining incompatible audio; the unimported remainder is reported. Ctrl+I gives exact byte ranges for each event. F6 gives the short version.

MP2 and ADTS AAC edits occur at frame boundaries of self-contained frames, which are copied byte-exactly and need no repacking. The importer walks headers strictly — no resynchronisation after damage, and a midstream change of profile, sample rate, or channel count stops the import and is reported in F6. ADTS CRC-protected frames and multi-block ADTS are unsupported. Playback and save validation decode through symphonia's MPEG Layer II and AAC decoders; AAC beyond AAC-LC in mono or stereo (such as HE-AAC/SBR) saves with structural validation only, and F6 says so on the file.

A malformed or truncated ID3v2 tag header no longer blocks audio recovery: only the tag header is skipped, then sync verification continues. A clean removal of whole frames at a frame boundary is not detectable from the remaining file, because the result is still a valid, shorter stream. The importer reports an intact sequence in that case rather than inventing a loss.

Exports contain a fresh seek index. Source tags, artwork, and encoder delay and padding metadata are omitted. A splice can change decoder filter history at the join, so the decoded samples immediately around a cut can differ from the original.

## Tests

Run `cargo test --locked` for the parser, recovery, report, ID3 tag inventory, and malformed-input tests. A seeded mutation test requires that damaged or random input never causes a panic, hang, or unbounded scan.

Real fixtures are generated locally from synthesized tones. They are not distributed with the project.

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\generate-fixtures.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\make-corruption-fixtures.ps1

# The fixture scripts write into the project's single build tree, resolved
# from cargo metadata.
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
| `cbr.mp2` | MPEG-1 Layer II, 44.1 kHz stereo |
| `cbr.aac` | ADTS AAC-LC, 44.1 kHz stereo |

The splice test writes `out-*.mp3` files in the fixture folder for independent decoder checks. The corruption script writes deterministic damaged copies and `manifest.txt` to the `corruptions` folder in the build tree. It covers a flipped header, inserted garbage, a short frame, a truncated final frame, missing start bytes, leading and trailing junk, a bogus ID3v2 size, an ID3v1 trailer, a midstream format change, clean whole-frame removal, an empty file, and random garbage. Clean whole-frame removal is a negative control: the result is a valid shorter stream and is reported as intact, because the file alone cannot prove that frames were removed.

An optional benchmark measures parse and validated-export time:

```powershell
$env:DIRECTCUT_BENCH_FIXTURE = 'target\bench\long-30min.mp3'
cargo test --release --locked -- --ignored export_benchmark --nocapture
```

Measured baseline details, limitations, and the full corruption matrix are in `docs/archive-audit-baseline.md`.
