# DirectCut RS

**Edit MP3, MP2, and AAC recordings without re-encoding them.**

DirectCut RS is a portable audio editor for Windows. It is designed for jobs such as removing mistakes from a recording, replacing a spoken passage with a better take, joining segments, and splitting a show or music mix into tracks using a CUE sheet. Edits are made to the compressed audio frames, so the audio you keep does not go through another lossy encoding pass.

The editor uses native Windows controls, supports keyboard operation and screen readers, and plays audio through WASAPI. It works with one continuous timeline rather than a multitrack session. When you cut or delete audio, the following material moves left to close the gap; when you insert audio, later material moves right. If you use REAPER, think of this as ripple editing that is always enabled.

> **Before you save:** DirectCut RS currently exports audio without the original ID3 or APEv2 tags, artwork, encoder delay, or padding metadata. An accompanying CUE sheet can be written, but not all fields from an existing CUE sheet are retained. **The opened source file and its original CUE sheet are never overwritten.** See [Saving and audio quality](#saving-and-audio-quality) for details.

## Getting started

1. Put `directcut-rs.exe` in a folder you can write to and run it. No installer is needed. Keep the executable's filename unchanged if you want to use automatic updates.
2. Select **Open Audio**, or press **Ctrl+O** in the opening window or an editor window. You can also open files from the command line. Each file gets its own editor window; use **Alt+Tab** to move between them.
3. Press **Space** to play. Use the **Left** and **Right** arrows to listen around an edit point, or **Ctrl+J** to jump to a specific time.
4. Press **[** at the beginning of the audio you want to select and **]** at the end. Press **Shift+Space** to preview that selection.
5. Press **Delete** to remove the selection, or use **Ctrl+X**, **Ctrl+C**, and **Ctrl+V** to cut, copy, and paste. Press **Ctrl+Z** if you want to undo an edit.
6. Press **Ctrl+S** to save. The first save asks where to put the edited copy; subsequent saves use that destination. DirectCut validates the exported stream before replacing the chosen destination. The original recording remains untouched.

In the opening window, **Ctrl+V** also opens supported audio files that you've copied in File Explorer. In an editor window, **F1** opens the keyboard shortcut reference, and **F6** opens the timeline status, including the playhead position, selection markers, current title, bitrate, and last operation.

## Everyday editing

### Remove something from a recording

Move to the beginning of the unwanted passage and press **[**. Move to its end and press **]**, then press **Delete**. The gap closes automatically. **Edit > Crop to Selection** does the opposite: it keeps the marked section and removes everything else. **Escape** clears both selection markers without changing any audio.

You can adjust an existing selection without resetting the markers: **Ctrl+[** and **Ctrl+]** move its start backward or forward; **Alt+[** and **Alt+]** move its end. Each move follows the current scrub step and plays a short preview so you can place the edit by ear. **Home** and **End** go to the selection boundaries; **Alt+Home** and **Alt+End** go to the start and end markers individually.

### Replace a bad take

Open the good take in another DirectCut window, select the passage you want, and press **Ctrl+C**. Switch to the original recording, mark the passage to replace, and press **Ctrl+V**. DirectCut replaces the marked audio; if no range is selected, it inserts the audio at the playhead instead. Its audio clipboard is shared across editor windows.

Replacing more than roughly ten seconds of selected audio triggers a confirmation, unless you've chosen **Don't ask again**. After a paste or file insertion, DirectCut announces what was added, where it went, and the new timeline length. Undo is available independently in each editor window.

### Insert or combine recordings

Press **Insert** or select **Insert Audio at Playhead...** to add another file at the current position. The files must use the same audio format, sample rate, and number of channels. Incoming audio contributes its sound, not its tags or artwork. **Ctrl+D** duplicates a marked section immediately after itself.

DirectCut asks before combining MP3 recordings whose average bitrates round to different numbers. For example, joining a 128 kbps segment to a 320 kbps segment keeps the audio from both without re-encoding and can produce a variable-bitrate file. If your player or broadcast system requires constant bitrate, check the exported file before using it.

If the timeline already contains CUE titles, DirectCut can offer to put a new cue point at the start of audio inserted from another file. It won't interrupt ordinary dialogue editing with this question when the timeline has no cues. More on this under [CUE sheets and track markers](#cue-sheets-and-track-markers).

### Save only part of a recording

Mark the range you want and choose **File > Save Selection As...**. This exports the selected audio without removing anything from the current timeline.

## Playback and finding edit points

**Space** starts playback, resumes a pause, or stops playback and returns to where playback began (or to the last pause point). **Ctrl+Space** pauses or resumes at the current position. If playback is stopped, it starts playing. For a quick check, **Shift+Space** previews the selected range and then stops.

Use **Left** and **Right** to scrub through the recording. By default, each press moves one compressed audio frame and plays approximately 0.1 seconds of audio beginning at the *new* playhead position. **Ctrl+Left** and **Ctrl+Right** move about one second at a time. The preview always plays at normal speed. If playback was already running, it normally resumes at the new position after the preview; uncheck **Transport > Continue Playback After Scrubbing** to prevent that. When stopped or paused, scrubbing plays only the preview.

The **-** and **=** keys adjust the size of each scrub move. Minus gives you finer control, down to one frame; equals increases the step, up to about one second. Press **/** to turn on **Loop Segment Scrub**. Instead of a single preview, DirectCut repeats a short piece starting at the playhead. The loop length matches the current scrub step, so you can move through a passage until the sound you want lands at the start of the loop. **Space** stops the loop and returns to its starting point. Press Space again to play from there. Enabling the loop while stopped starts it immediately, and the setting is remembered.

**Ctrl+J** jumps to a time entered as seconds (`90`), minutes and seconds (`1:30`), or hours, minutes, and seconds (`1:02:30`). It also accepts a CUE title number prefixed with `t`, such as `t24`. Time jumps snap to the nearest supported audio-frame boundary; a time beyond the recording goes to its end. **Ctrl+Home** and **Ctrl+End** go straight to the beginning or end.

Playback speed ranges from **0.25x to 4.00x**, in 0.25x steps. Use **Shift+-** or **Shift+=**, or choose a speed from the **Transport** menu; **Ctrl+Shift+Backspace** returns to 1.00x. This is *varispeed*: pitch changes with speed. It is not pitch-preserving time stretching. The timeline positions and cue points still follow the audio correctly.

Under **Transport > Playback Device**, you can select **System Default** or another Windows audio output device. The choice applies to the current editor window. If that device disappears, playback reports an error rather than silently switching outputs.

A visual timeline shows the playhead, selection markers, and CUE points as the recording plays. It doesn't take keyboard focus. For screen readers, use **F6** to review the equivalent timeline information, **Ctrl+Shift+J** to hear the playhead position and current title, and **Ctrl+Shift+Space** to hear the selected range.

## CUE sheets and track markers

A **CUE sheet** is a text file that identifies where tracks or sections start inside a longer recording. If you open an MP3, MP2, or ADTS AAC file with a matching `.cue` file beside it, DirectCut loads its track titles and start positions. You can also add your own cue points: move to the desired place and press **Ctrl+Shift+N** or the unmodified **S** key. A name prompt opens, with a suggested name based on the current title; a blank name becomes `Untitled`.

Use **Ctrl+Shift+Left** or **Ctrl+Shift+Right** to move between titles, or **Ctrl+Shift+T** to go to a title by number or part of its name. Title navigation works relative to the title currently containing the playhead. If there is no next title, the playhead doesn't move. **Titles > Read Title List** gives you each title's start and calculated duration. If you want the end of the whole recording, use **Ctrl+End**, not Next Title.

### Keeping track of inserted segments

When a timeline already has cue points, DirectCut offers to add one at the start of audio brought in from another source, provided there isn't already one at that position. The suggested name is `$file`. This displays the name of the file that supplied the audio at that cue point, without its extension. For instance, a cue named `Intro - $file` on material from `Part 2.mp3` appears as **Intro - Part 2**. If you move that cue onto audio from another file, the displayed name changes accordingly.

The names are used in the title list, saved CUE sheet, and individual files created by a split. You can decline a suggested boundary; the insertion still happens. If there are no cues in the timeline, DirectCut doesn't offer to create them automatically.

### Editing and splitting titles

Use **Titles > Cue Point** to rename, delete, or move a cue point. **Ctrl+Shift+R** renames the cue that contains the playhead, **Ctrl+Shift+D** deletes it, and **Ctrl+Shift+M** moves the nearest cue to the playhead. **Ctrl+Shift+B** and **Ctrl+Shift+F** shift all cues backward or forward by about one second, useful when a station's title changes consistently lead or lag the recording. If a shift would cause cues to meet at a timeline boundary, DirectCut asks before merging them; a merge keeps the later cue.

Cue points move with the audio as you edit. A cue inside a cut is removed, and later cues move earlier. A cue right where the cut starts can stay at that spot. Cropping preserves cues within the kept audio and creates a title at the new beginning if the crop starts inside an existing title. All cue edits can be undone.

Choose **Titles > Split Audio at Cue Points...** to export each title as its own file. The output names contain the track number and displayed title, such as `01 Intro - show.mp3`. Each track runs to the next cue, and the final track runs to the end of the recording. DirectCut warns if audio before the first cue would be omitted. Existing files are protected: if a name is already taken, DirectCut creates a numbered alternative rather than replacing it. The split does not re-encode the audio.

**Titles > Announce Titles While Playing** speaks track titles as playback reaches them. This can be particularly useful for checking long radio archives or mixes without repeatedly opening the title list.

A couple of positioning details are worth knowing. Immediately after jumping to a title, **[** or **]** places its marker at the title's beginning; nudge with an arrow key first to mark a position within the title. If you move the start marker to or past the end marker, DirectCut clears the end marker, and vice versa, to avoid an invalid selection. **Escape** clears the time selection but not any cue points.

When you save audio with titles, DirectCut writes a matching CUE sheet beside the exported file. It also creates a CUE sheet for **Save Selection As** when applicable. The output CUE sheet contains `FILE`, `TRACK`, `TITLE`, and `INDEX 01`; other original entries, including `PERFORMER` and `REM`, are not carried over.

## Working with existing archives

### Trim an archive by title

Use **Trim Archive...** when you have a long MP3 archive with an accompanying CUE sheet and want to retain only a range of tracks. Select the first and last title to keep. DirectCut trims up to the *next* title boundary after your last selection, or to the end of the recording if it was the final title.

The result is a new file such as `show_trimmed.mp3`, with a matching CUE sheet. If the archive name ends in a part number such as `_part3` and there is a compatible `titles.csv` beside it, DirectCut also writes a trimmed title CSV. The original archive stays untouched. If an output name is taken, DirectCut tries a numbered name such as `show_trimmed_2.mp3`. The trim preserves compressed frames without re-encoding them. Because CUE timings are stored at 75 positions per second while MP3 editing uses audio-frame boundaries, the result is frame-accurate rather than sample-accurate.

### Inspect a damaged MP3

Choose **Analyze Source MP3...** or press **Ctrl+I**. This scans the opened MP3 without changing it and opens a readable text report that you can save. It describes how much audio was retained, where invalid or missing material was detected, how many bytes or frames were skipped, and the file's SHA-256 hash. The report also identifies readable ID3 tags, artwork presence, and ReplayGain entries. **F6** gives a short recovery summary for imported audio that contained gaps.

DirectCut can resume reading an MP3 after certain kinds of corruption by looking for a sequence of valid frames. It discards unusable or incomplete data and places the surviving audio together; it does **not** synthesize missing sound or insert silence. The report distinguishes exact byte locations of problems from estimates of missing audio. This is a structural inspection, **not** a full audio decode or a guarantee that every audible defect will be found. In particular, a clean removal of complete MP3 frames can leave a perfectly valid but shorter file, which cannot be identified as damaged from the remaining bytes alone.

The import stops rather than joining audio if the MPEG version, sample rate, or channel count changes partway through a file. A badly damaged Xing/Info/VBRI header is another known limitation: it can make an index frame appear to be a short audio frame. See `docs/archive-audit-baseline.md` for the current recovery tests and limitations. Analysis applies to **MP3 only**, not MP2 or AAC.

For command-line use:

```powershell
.\target\release\directcut-rs.exe --analyze .\archive.mp3
.\target\release\directcut-rs.exe --analyze .\archive.mp3 --out ".\archive - analysis.txt"
```

Without `--out`, the report is printed to the console. An existing report will not be overwritten unless you add `--force`. DirectCut rejects the source path as the output path. If you use `--force`, make sure the output is a separate file, since DirectCut cannot detect a hard link to the source.

## Saving and audio quality

**What “lossless editing” means here:** DirectCut cuts, copies, inserts, and joins *compressed audio frames* without sending the retained audio through an MP3, MP2, or AAC encoder again. This avoids the quality loss of an additional lossy encoding pass. The output file itself need not be byte-for-byte identical: DirectCut may rebuild its frame layout, transport headers, and seek information. Playback also decodes audio for listening, and supported formats are decoded during export validation; that does not mean the export is re-encoded.

There are two important qualifications. First, edits are aligned to compressed audio frames, **not individual PCM samples**. Second, the decoder's history at a join can change, so a few decoded samples around a cut may sound or measure slightly different even though the audio data you retained was not re-encoded.

**Metadata is not preserved.** Exports omit ID3v1, ID3v2, APEv2, embedded artwork, and encoder delay and padding information. That also applies to archives trimmed by title. The app can inspect certain tags, but it cannot yet preserve them on export. The source file and source CUE sheet remain protected, and a new CUE sheet is written when the output has titles.

**No fades, gain changes, or normalization.** Those operations would change decoded samples. On MP3, MP2, and AAC, doing that would either require another lossy encode or a quantized approximation, so DirectCut does not pretend such operations are lossless.

### Supported formats and limitations

| Format | Editing and playback notes |
| --- | --- |
| MP3 (MPEG Layer III) | Supports MPEG-1, MPEG-2, and MPEG-2.5 using standard bitrate indices. Rebuilds the bit reservoir and seek index when needed. Free-format MP3 and midstream format changes are not supported. |
| MP2 (MPEG Layer II) | Edits complete frames without re-encoding. Import stops at damaged frames or incompatible changes within the stream; it does not attempt to resynchronize beyond them. |
| ADTS AAC | Edits complete frames without re-encoding. AAC-LC mono and stereo are supported for decoding and validation. Other streams, such as HE-AAC/SBR, can still be edited but are saved with structural validation only. CRC-protected and multi-block ADTS frames are unsupported. |

For all three formats, pasting and inserting require matching format, sample rate, and channel count. DirectCut asks before combining MP3s with different rounded average bitrates. During export, an individual frame's bitrate may need to increase to make room for its retained compressed audio. Rarely, an MP3 edit cannot fit within the available bit-reservoir limits; in that case, adjust the cut point or undo the operation.

DirectCut's MP3 recovery skips invalid bytes, damaged frames, and frames whose required bit-reservoir history is no longer available. MP2 and AAC take the stricter approach of stopping at the first damaged or incompatible frame. See the analysis report for the details of an MP3 import and **F6** for a concise status in the editor.

## Keyboard shortcut reference

In an editor window, press **F1** or choose **Help > Keyboard Shortcuts** to read the editor shortcuts as plain text or save them to a file. The tables also include shortcuts for the opening window. On a non-US keyboard layout, use the keys in the US-layout positions for punctuation shortcuts such as **[** and **-**.

### Transport and navigation

| Keys | Action |
| --- | --- |
| Space | Play or resume, or stop and return to the playback start or last pause point |
| Ctrl+Space | Pause or resume, or start playback when stopped |
| Shift+Space | Preview the selection |
| Ctrl+J | Jump to a time or title (`t24`) |
| Ctrl+Home / Ctrl+End | Beginning / end of recording |
| Left / Right | Scrub backward / forward by the current step |
| Ctrl+Left / Ctrl+Right | Scrub backward / forward by about one second |
| - / = | Decrease / increase scrub step |
| / | Toggle loop segment scrub |
| Shift+- / Shift+= | Decrease / increase playback speed |
| Ctrl+Shift+Backspace | Restore normal playback speed |
| Ctrl+Shift+J | Report playhead position and current title |
| Ctrl+Shift+Space | Report selected time range |

### Selecting and editing

| Keys or command | Action |
| --- | --- |
| [ / ] | Mark selection start / end |
| Ctrl+[ / Ctrl+] | Move start marker backward / forward by one scrub step |
| Alt+[ / Alt+] | Move end marker backward / forward by one scrub step |
| Alt+Home / Alt+End | Jump to start / end marker |
| Home / End | Jump to selection start / end |
| Escape | Clear the selection |
| Ctrl+A | Select the entire recording |
| Ctrl+X / Ctrl+C / Ctrl+V | Cut / copy / paste audio |
| Delete | Remove selected audio and close the gap |
| Ctrl+D | Duplicate selected audio after itself |
| Insert | Insert another file at the playhead |
| Edit > Crop to Selection | Keep only the selection |
| File > Save Selection As... | Export only the selection |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+S / Ctrl+Shift+S | Save / Save As |

### CUE points and reports

| Keys or command | Action |
| --- | --- |
| Ctrl+Shift+Left / Ctrl+Shift+Right | Previous / next title |
| Ctrl+Shift+T | Go to title by number or name |
| Ctrl+Shift+N or S | Add cue point at playhead |
| Ctrl+Shift+R | Rename current cue point |
| Ctrl+Shift+D | Delete current cue point |
| Ctrl+Shift+M | Move nearest cue point to playhead |
| Ctrl+Shift+B / Ctrl+Shift+F | Move all cues back / forward about one second |
| Titles > Read Title List | Review title starts and lengths |
| Titles > Split Audio at Cue Points... | Export one file per title |
| Ctrl+I | Analyze the source MP3 |
| F6 | View timeline status and recovery summary |
| F1 | Read or save all keyboard shortcuts |

### Opening and closing files

| Keys | Action |
| --- | --- |
| Ctrl+O | Open audio in the opening window or an editor window |
| Ctrl+W in an editor window | Close that editor window |
| Ctrl+V in the opening window | Open audio files copied in File Explorer |

## Accessibility

DirectCut announces title changes, jumps, marker positions, selection lengths, edits, pasted audio, and completed saves through Windows UI Automation notifications. It does not need a separate NVDA or JAWS add-on, and it does not speak through SAPI or another fallback voice. The **F6** timeline status and the keyboard-accessible reports remain available whether or not a screen reader is running.

When dialogs close, DirectCut attempts to restore keyboard focus to the control you were using beforehand. This matters during keyboard scrubbing and editing: you should not have to find your way back from an unrelated field after every prompt. The drawn timeline never takes focus.

Accessibility notifications require **Windows 10 version 1709 or later**. They have been tested with NVDA on Windows 10 22H2; behavior with JAWS and Narrator should be checked with the versions you use.

## Portable settings and updates

DirectCut stores `settings.toml` beside `directcut-rs.exe` for application settings such as playback speed, device, and scrub zoom. The `.freshen` folder in the same location holds update downloads and recovery information. It does not place these files in AppData or the Windows temporary folder. **Keep the program in a writable folder**; otherwise, saving preferences or installing an update reports an error.

The program checks GitHub for signed releases when it starts. To check manually, choose **About > Check for updates**. The updater shows release notes, download progress, and installation status. Before choosing **Install update**, save your work and close all editor windows and any other DirectCut RS instances. The update helper replaces the executable after the app exits and restarts DirectCut. The helper closes on success, and the restarted app reports when Freshen has finished the installation.

## For developers

DirectCut RS is written in Rust, with RunGUI native Windows controls and WASAPI playback. Build it using the Rust MSVC toolchain and Visual Studio C++ Build Tools:

```powershell
cargo build --release --locked
.\target\release\directcut-rs.exe
```

Run the parser, recovery, metadata, and malformed-input tests with:

```powershell
cargo test --locked
```

Additional tests use generated synthetic recordings rather than distributing copyrighted or personal audio. To generate them, put FFmpeg with `libmp3lame` and the separate `lame` command on your PATH. Then run:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\generate-fixtures.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\make-corruption-fixtures.ps1

$env:DIRECTCUT_TEST_FIXTURES = 'target\fixtures'
$env:DIRECTCUT_TEST_CORRUPTIONS = 'target\corruptions'
cargo test --locked -- --ignored --skip export_benchmark
```

Fixtures include MPEG-1 CBR and VBR MP3s, MPEG-2 and MPEG-2.5 MP3s, a protected-bit MP3, an MP3 with embedded artwork, MPEG-1 Layer II audio, and ADTS AAC-LC. The corruption tests cover truncated or damaged frames, misleading headers, junk data, midstream format changes, and random input. Deliberately removing complete frames is included as a negative control because the resulting shorter stream can still be structurally valid. Splice tests create `out-*.mp3` files for independent decoder checks.

An optional export benchmark uses:

```powershell
$env:DIRECTCUT_BENCH_FIXTURE = 'target\bench\long-30min.mp3'
cargo test --release --locked -- --ignored export_benchmark --nocapture
```

The detailed test matrix, measured baselines, and remaining recovery limitations are recorded in [`docs/archive-audit-baseline.md`](docs/archive-audit-baseline.md).
