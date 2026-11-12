use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::{self, Receiver, TryRecvError},
};

use rungui::*;

use crate::{
    OpenDocument, Session, announce,
    audio::{self, Playback},
    codec,
    codec::{Clipboard, Frames, Info, Timeline},
    editkeys, focus,
    mp3::Result,
    report, settings, stream, timeline as timeline_control,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct TitleMarker {
    frame: usize,
    title: String,
}

/// The name CUE titles get when the sheet does not name them; manually added
/// cue points start here too.
const UNTITLED: &str = "Untitled";

/// The cue point name token that stands for the file the audio at the cue
/// point came from. It stays in the stored name and is replaced everywhere
/// the name is shown, written, or used, so a name follows its audio through
/// pastes and splits: a cue point added inside one file's audio keeps
/// naming that file even after the audio is copied into another window.
const FILE_TOKEN: &str = "$file";

/// The display name for a file: without its extension, the form a
/// track name reads.
fn source_name(path: &Path) -> String {
    path.file_stem()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Keep the path as the identity, not just its display name.
fn source_identity(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The file identity that owns the frame `frame`, or `None` when the timeline
/// has no source information. A span starting exactly at `frame` owns it:
/// a cue point at the start of pasted audio names the file that was
/// pasted, which is the rule the boundary cue point is named by.
fn source_at(sources: &[(usize, String)], frame: usize) -> Option<&str> {
    let index = sources.partition_point(|(start, _)| *start <= frame);
    index.checked_sub(1).map(|index| sources[index].1.as_str())
}

/// Source spans for a crop of `range`, rebased so the crop starts at 0.
/// The file owning the range's first frame owns the crop's opening frames;
/// entries strictly inside the range are rebased the way
/// [`titles_in_range`] rebases cue points.
fn sources_in_range(
    sources: &[(usize, String)],
    range: &std::ops::Range<usize>,
) -> Vec<(usize, String)> {
    let mut result = Vec::new();
    if range.end > range.start
        && let Some(label) = source_at(sources, range.start)
    {
        result.push((0, label.to_string()));
    }
    for (frame, label) in sources
        .iter()
        .filter(|(frame, _)| *frame > range.start && *frame < range.end)
    {
        result.push((frame - range.start, label.clone()));
    }
    result
}

/// Move source spans through a frame splice, the way [`splice_titles`]
/// moves cue points, with one deliberate difference: a cue point at the
/// splice start stays put to mark the audio that begins there, while a
/// source span marks what its frames ARE, so spans are rebuilt from the
/// surviving ranges — the labels before the splice, the inserted block's
/// labels, then the labels after the splice, shifted — and runs of equal
/// labels are compressed back into spans. `inserted` is relative to the
/// block's start, its first entry at frame 0, and `added` is its length in
/// frames.
fn splice_spans(
    sources: &mut Vec<(usize, String)>,
    old_total: usize,
    start: usize,
    end: usize,
    inserted: &[(usize, String)],
    added: usize,
) {
    let mut rebuilt = sources_in_range(sources, &(0..start));
    for (frame, label) in inserted {
        rebuilt.push((start + frame, label.clone()));
    }
    for (frame, label) in sources_in_range(sources, &(end..old_total)) {
        rebuilt.push((frame + start + added, label));
    }
    rebuilt.sort_by_key(|(frame, _)| *frame);
    rebuilt.dedup_by(|later, earlier| later.1 == earlier.1);
    *sources = rebuilt;
}

/// The cue point name as shown, written, and used: `$file` inside a name
/// stands for the file the audio at the cue point came from. A timeline
/// without source information expands the token to an empty string.
fn expanded_title(title: &str, sources: &[(usize, String)], frame: usize) -> String {
    if !title.contains(FILE_TOKEN) {
        return title.to_string();
    }
    let file = source_at(sources, frame)
        .map(|path| source_name(Path::new(path)))
        .unwrap_or_default();
    title.replace(FILE_TOKEN, &file)
}

fn check_splice_format(destination: Info, incoming: Info) -> Result<()> {
    if incoming.rate() != destination.rate() {
        return Err(format!(
            "Sample rates must match. The incoming audio is {} Hz and the timeline is {} Hz. DirectCut does not resample audio.",
            incoming.rate(),
            destination.rate()
        )
        .into());
    }
    if incoming != destination {
        return Err(
            "The incoming audio must match the timeline's format and channel count.".into(),
        );
    }
    Ok(())
}

/// Whether two average bitrates differ at whole-kbps precision. A steady
/// 128 kbps file and a VBR stream averaging 128.03 are the same rate to a
/// listener; 128 and 192 are not.
fn bitrates_differ(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => (left.round() as i64) != (right.round() as i64),
        _ => false,
    }
}

/// The spoken and displayed form of an average bitrate, such as `128 kbps`.
fn kbps_text(kbps: Option<f64>) -> String {
    match kbps {
        Some(kbps) => format!("{kbps:.0} kbps"),
        None => "an unknown bitrate".to_string(),
    }
}

/// A marked range longer than this many seconds is confirmed before a
/// paste replaces it. A range that long is usually the audio a previous
/// paste or insert left marked rather than something the user is aiming
/// at, and replacing it silently is how whole takes vanish; a shorter
/// range is the ordinary corrected-take edit and is not asked about.
const REPLACE_ASK_SECONDS: f64 = 10.0;

/// Whether a paste over a marked range of `length` frames asks before it
/// replaces.
fn replace_needs_confirm(length: usize, format: Info) -> bool {
    let seconds = length as f64 * format.samples() as f64 / f64::from(format.rate());
    seconds > REPLACE_ASK_SECONDS
}

/// Whether to offer a cue point at a just-combined file boundary: the
/// timeline already has cue points (dialog combining has no sheet to
/// respect, so nothing is asked), no cue point already starts exactly
/// there, and the boundary lies inside the audio — a boundary at the very
/// end would make a cue point with no track to name.
fn boundary_ask_wanted(titles: &[TitleMarker], frame: usize, total: usize) -> bool {
    frame < total && !titles.is_empty() && !titles.iter().any(|marker| marker.frame == frame)
}

/// What the jump dialog starts with: `t` and the title number when the
/// playhead sits at a title boundary — right after a title jump it is the
/// jumped-to title, and a plain seek onto a cue point's exact frame is
/// that title — otherwise the playhead's own time, so nudging it and
/// jumping again is a matter of editing the digits.
fn jump_initial_text(
    titles: &[TitleMarker],
    format: Info,
    anchor: Option<usize>,
    position: usize,
) -> String {
    if let Some(anchor) = anchor
        && marker_frame(titles, format, Some(anchor), position) == anchor
        && let Some(index) = titles.iter().position(|marker| marker.frame == anchor)
    {
        return format!("t{}", index + 1);
    }
    if let Some(index) = titles.iter().position(|marker| marker.frame == position) {
        return format!("t{}", index + 1);
    }
    time_text(format, position)
}

#[derive(Clone)]
struct Edit {
    frames: Frames,
    titles: Vec<TitleMarker>,
    /// Which file each run of frames came from: sorted `(frame, source path)`
    /// entries, the first always at frame 0, each entry covering the frames
    /// up to the next one. Cue point names carrying `$file` resolve against
    /// this, so it is kept exact through every edit the way the frames and
    /// the titles are.
    sources: Vec<(usize, String)>,
    cursor: usize,
    start: Option<usize>,
    end: Option<usize>,
    revision: u64,
}

impl Edit {
    fn selection(&self) -> Result<std::ops::Range<usize>> {
        match (self.start, self.end) {
            (Some(start), Some(end)) if start < end => Ok(start..end),
            _ => Err("Set a start marker and an end marker after it.".into()),
        }
    }
}

const SPEED_MIN: f32 = 0.25;
const SPEED_MAX: f32 = 4.0;

/// A scrub move plays about this much audio from the new position, so moving
/// the playhead is audible even while playback is stopped.
const SCRUB_SNAP_SECONDS: f64 = 0.1;

fn time_text(format: Info, frame: usize) -> String {
    let millis = audio::duration(format, frame).as_millis();
    format!(
        "{}:{:02}.{:03}",
        millis / 60_000,
        millis / 1000 % 60,
        millis % 1000
    )
}

fn frame_seconds(format: Info, frame: usize) -> f64 {
    frame as f64 * format.samples() as f64 / f64::from(format.rate())
}

/// Frames per second, rounded up. This is the largest scrub step: one second.
fn frames_per_second(format: Info) -> usize {
    (format.rate() as usize).div_ceil(format.samples())
}

/// The playback speeds offered in the Transport menu, from the minimum to the
/// maximum in 0.25x steps.
fn speed_steps() -> impl Iterator<Item = f32> {
    (0..16).map(|step| SPEED_MIN + 0.25 * step as f32)
}

/// The number of frames in a scrub snippet: about 0.1 seconds, at least one.
fn scrub_snap_frames(format: Info) -> usize {
    ((SCRUB_SNAP_SECONDS * f64::from(format.rate()) / format.samples() as f64).ceil() as usize)
        .max(1)
}

/// The spoken and displayed form of a scrub step, such as `1 frame (26 ms)`.
fn scrub_text(step: usize, format: Info) -> String {
    let millis =
        (step as f64 * format.samples() as f64 / f64::from(format.rate()) * 1000.0).round() as u64;
    if step == 1 {
        format!("1 frame ({millis} ms)")
    } else {
        format!("{step} frames ({millis} ms)")
    }
}

/// The scrub step after zooming. Zooming in halves the step, stopping at one
/// frame; zooming out doubles it. The caller clamps to one second of frames.
fn zoomed_scrub_step(step: usize, zoom_in: bool) -> usize {
    if zoom_in { (step / 2).max(1) } else { step * 2 }
}

/// Convert CUE start times into frame-aligned title markers. Entries outside the
/// timeline are dropped. If two titles share a frame, the later title wins,
/// which is the same rule used when an edit deletes a whole title.
fn cue_titles(entries: Vec<(f64, String)>, format: Info, total: usize) -> Vec<TitleMarker> {
    let mut markers = Vec::with_capacity(entries.len());
    for (seconds, title) in entries {
        if total == 0 || !seconds.is_finite() || seconds < 0.0 {
            continue;
        }
        let frame = (seconds * f64::from(format.rate()) / format.samples() as f64).round();
        if frame >= total as f64 {
            continue;
        }
        let title = title.trim();
        markers.push(TitleMarker {
            frame: frame as usize,
            title: title_name(title).into(),
        });
    }
    markers.sort_by_key(|marker| marker.frame);
    dedupe_titles(markers)
}

fn dedupe_titles(markers: Vec<TitleMarker>) -> Vec<TitleMarker> {
    let mut result: Vec<TitleMarker> = Vec::with_capacity(markers.len());
    for marker in markers {
        if result.last().is_some_and(|last| last.frame == marker.frame) {
            result.pop();
        }
        result.push(marker);
    }
    result
}

/// Move markers through a frame splice. A marker at the splice start survives;
/// titles strictly inside the replaced range disappear; later titles shift by
/// the length change. Duplicate starts keep the later title.
fn splice_titles(
    titles: &mut Vec<TitleMarker>,
    start: usize,
    end: usize,
    inserted: usize,
    total: usize,
) {
    let delta = inserted as isize - (end - start) as isize;
    let mut moved = Vec::with_capacity(titles.len());
    for marker in titles.drain(..) {
        let frame = if marker.frame <= start {
            marker.frame
        } else if marker.frame >= end {
            marker.frame.saturating_add_signed(delta)
        } else {
            continue;
        };
        if frame < total {
            moved.push(TitleMarker {
                frame,
                title: marker.title,
            });
        }
    }
    *titles = dedupe_titles(moved);
}

/// The frame ranges a split writes: one file per CUE title, each running to
/// the next title, and the last to the end of the timeline. Audio before the
/// first cue point belongs to no track and is left out, exactly as Trim
/// Archive leaves it out.
fn split_ranges(titles: &[TitleMarker], total: usize) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::with_capacity(titles.len());
    for (index, marker) in titles.iter().enumerate() {
        if marker.frame >= total {
            break;
        }
        let end = titles
            .get(index + 1)
            .map_or(total, |next| next.frame.min(total));
        if end > marker.frame {
            ranges.push(marker.frame..end);
        }
    }
    ranges
}

/// A path that does not exist: the first free numbered sibling of `path`.
fn safe_name(text: &str, fallback: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            cleaned.push('_');
        } else {
            cleaned.push(ch);
        }
    }
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed
        .trim_start_matches('-')
        .trim_matches(['.', ' '])
        .to_string();
    let mut name: String = if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
    .chars()
    .take(100)
    .collect();
    name = name.trim_end_matches([' ', '.']).to_string();
    if name.is_empty() {
        return fallback.to_string();
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        name.insert(0, '_');
    }
    name
}

fn unique_split_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().map_or_else(
        || "track".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let extension = path.extension().map_or_else(String::new, |extension| {
        extension.to_string_lossy().into_owned()
    });
    let parent = path
        .parent()
        .map_or_else(|| PathBuf::new(), Path::to_path_buf);
    for suffix in 2.. {
        let candidate = parent.join(format!("{stem} {suffix}.{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("a free file name always exists");
}

/// Write one split track to the first free numbered sibling of `suggested`,
/// never replacing an existing file. The export claims the name atomically
/// just before committing, so a racing writer or a foreign file is never
/// overwritten; a lost claim retries with the next free numbered name.
fn export_split_file(
    suggested: PathBuf,
    format: Info,
    frames: Frames,
) -> std::result::Result<PathBuf, String> {
    for _ in 0..1000 {
        let candidate = unique_split_path(suggested.clone());
        match audio::export_no_replace(&candidate, format, frames.clone()) {
            Ok(audio::NoReplace::Written(path)) => return Ok(path),
            Ok(audio::NoReplace::Existed) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("Every numbered name for this track is already in use.".into())
}

/// Titles that survive cropping to `range`, rebased so the crop starts at 0.
/// A crop that starts inside a title synthesizes that title at the new start.
fn titles_in_range(titles: &[TitleMarker], range: &std::ops::Range<usize>) -> Vec<TitleMarker> {
    let mut result = Vec::new();
    if let Some(holder) = titles
        .iter()
        .rev()
        .find(|marker| marker.frame <= range.start)
    {
        result.push(TitleMarker {
            frame: 0,
            title: holder.title.clone(),
        });
    }
    for marker in titles
        .iter()
        .filter(|marker| marker.frame > range.start && marker.frame < range.end)
    {
        result.push(TitleMarker {
            frame: marker.frame - range.start,
            title: marker.title.clone(),
        });
    }
    result
}

fn cue_entries(
    titles: &[TitleMarker],
    sources: &[(usize, String)],
    format: Info,
    total: usize,
) -> Vec<(f64, String)> {
    titles
        .iter()
        .filter(|marker| marker.frame < total)
        .map(|marker| {
            (
                frame_seconds(format, marker.frame),
                expanded_title(&marker.title, sources, marker.frame),
            )
        })
        .collect()
}

/// The stored form of a cue point name: trimmed, with a blank entry becoming
/// the placeholder name. CUE sheets can carry empty titles; cue points added
/// here start from this rule too.
fn title_name(text: &str) -> &str {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        UNTITLED
    } else {
        trimmed
    }
}

/// Sorted insertion of a new cue point. The caller refuses frames that hold
/// another cue point or lie at the end of the audio, so this only places.
/// Returns where the marker was inserted.
fn insert_title(titles: &mut Vec<TitleMarker>, frame: usize, title: &str) -> usize {
    let position = titles.partition_point(|marker| marker.frame < frame);
    titles.insert(
        position,
        TitleMarker {
            frame,
            title: title_name(title).into(),
        },
    );
    position
}

/// Index of the cue point nearest `frame`. A tie goes to the earlier title,
/// which is the one the playhead has most recently passed.
fn nearest_title_index(titles: &[TitleMarker], frame: usize) -> Option<usize> {
    titles
        .iter()
        .enumerate()
        .min_by_key(|(index, marker)| (marker.frame.abs_diff(frame), *index))
        .map(|(index, _)| index)
}

/// Move every cue point by `delta` frames, clamped into the timeline. Cue
/// points that meet keep the later one, the same rule every other title move
/// uses. Order cannot invert when every marker moves by the same amount, so
/// no sort is needed.
fn shifted_titles(titles: &[TitleMarker], delta: isize, total: usize) -> Vec<TitleMarker> {
    let last = total.saturating_sub(1);
    dedupe_titles(
        titles
            .iter()
            .map(|marker| TitleMarker {
                frame: marker.frame.saturating_add_signed(delta).min(last),
                title: marker.title.clone(),
            })
            .collect(),
    )
}

/// The title containing `frame`: the last marker at or before it, if any.
fn title_at_frame(titles: &[TitleMarker], frame: usize) -> Option<usize> {
    titles.iter().rposition(|marker| marker.frame <= frame)
}

/// Index of the title before the one containing `frame`. Playback may already
/// have moved past a title start, so navigation is relative to the containing
/// title, not the raw playhead frame. At or before the first title this is
/// `None`, meaning go to the start of the timeline.
fn previous_title_index(titles: &[TitleMarker], frame: usize, total: usize) -> Option<usize> {
    if titles.is_empty() {
        return None;
    }
    if frame >= total {
        return Some(titles.len() - 1);
    }
    match title_at_frame(titles, frame) {
        Some(0) | None => None,
        Some(index) => Some(index - 1),
    }
}

/// Index of the title after the one containing `frame`. Before the first title
/// this is the first title; at or past the end of the timeline it is `None`,
/// meaning go to the end of the timeline.
fn next_title_index(titles: &[TitleMarker], frame: usize, total: usize) -> Option<usize> {
    if titles.is_empty() || frame >= total {
        return None;
    }
    match title_at_frame(titles, frame) {
        Some(index) => (index + 1 < titles.len()).then_some(index + 1),
        None => Some(0),
    }
}

/// How long after a title jump a marker command still snaps to that title's
/// start. Long enough to react to the jump announcement, short enough that a
/// deliberate mid-title marker is not hijacked.
const TITLE_MARKER_SNAP_SECONDS: f64 = 2.0;

/// The frame a marker command should use. Right after a title jump the playhead
/// has already moved past the title start, so use the navigation anchor while
/// the playhead is still inside that title and close to it.
fn marker_frame(
    titles: &[TitleMarker],
    format: Info,
    anchor: Option<usize>,
    position: usize,
) -> usize {
    let Some(anchor) = anchor else {
        return position;
    };
    let limit = (TITLE_MARKER_SNAP_SECONDS * f64::from(format.rate()) / format.samples() as f64)
        .round() as usize;
    if position >= anchor
        && position - anchor <= limit
        && title_at_frame(titles, anchor) == title_at_frame(titles, position)
    {
        anchor
    } else {
        position
    }
}

#[derive(Debug, PartialEq)]
enum JumpTarget {
    Time(f64),
    Title(usize),
}

fn parse_jump(text: &str) -> Result<JumpTarget> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Enter a time, or t and a title number such as t24.".into());
    }
    if text.len() > 1 && matches!(text.as_bytes()[0], b't' | b'T') {
        let number: usize = text[1..]
            .trim()
            .parse()
            .map_err(|_| "Use t and a title number, for example t24.".to_string())?;
        if number == 0 {
            return Err("Title numbers start at 1.".into());
        }
        return Ok(JumpTarget::Title(number));
    }
    parse_timestamp(text).map(JumpTarget::Time)
}

/// Parse `ss`, `mm:ss`, or `hh:mm:ss` into seconds. Fractions are allowed in
/// the final part. A bare seconds value may exceed 59.
fn parse_timestamp(text: &str) -> Result<f64> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Enter a time as ss, mm:ss, or hh:mm:ss.".into());
    }
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() > 3 {
        return Err("Use ss, mm:ss, or hh:mm:ss.".into());
    }
    let mut values = Vec::with_capacity(parts.len());
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            return Err("Each time part needs a number.".into());
        }
        let value: f64 = part
            .parse()
            .map_err(|_| format!("\"{part}\" is not a number."))?;
        if !value.is_finite() || value < 0.0 {
            return Err("Time values must be zero or greater.".into());
        }
        values.push(value);
    }
    let seconds = match values.as_slice() {
        [seconds] => *seconds,
        [minutes, seconds] => {
            if *seconds >= 60.0 {
                return Err("Seconds must be less than 60 in mm:ss.".into());
            }
            minutes * 60.0 + seconds
        }
        [hours, minutes, seconds] => {
            if *minutes >= 60.0 || *seconds >= 60.0 {
                return Err("Minutes and seconds must be less than 60 in hh:mm:ss.".into());
            }
            hours * 3600.0 + minutes * 60.0 + seconds
        }
        _ => unreachable!(),
    };
    Ok(seconds)
}

struct Insertion {
    path: PathBuf,
    /// The timeline revision when the insertion was requested, so an edit
    /// made while the file is read invalidates the insertion instead of
    /// landing at a position that no longer means what it did.
    revision: u64,
    position: usize,
    result: Result<Timeline>,
}

/// One split export: the name suggested for the track, its title number and
/// text, the source frame range, and the path actually written or the
/// failure. The written path can differ from the suggestion when the
/// suggested name was already in use.
struct SplitOutcome {
    suggested: PathBuf,
    number: usize,
    title: String,
    range: std::ops::Range<usize>,
    result: std::result::Result<PathBuf, String>,
}

/// What the split worker reports: the source it split, the folder it wrote
/// into, and one outcome per track in track order.
struct SplitMessage {
    source: PathBuf,
    folder: PathBuf,
    outcomes: Vec<SplitOutcome>,
}

struct Opened {
    timeline: Timeline,
    cue: Option<PathBuf>,
    cue_entries: Vec<(f64, String)>,
    cue_error: Option<String>,
}

struct ExportOutcome {
    written_cue: Option<(PathBuf, Vec<u8>)>,
    cue: Option<PathBuf>,
    /// An obsolete sidecar this window wrote earlier, removed on this save
    /// because the timeline no longer has any title markers.
    removed_cue: Option<PathBuf>,
    /// A failure that leaves the destination not matching the document: the
    /// CUE sheet could not be written, or a foreign sheet was preserved. The
    /// save stays incomplete until this is resolved.
    cue_error: Option<String>,
    /// Expected but empty: markers were expected and none remain. Pure
    /// information, not a failure.
    cue_note: Option<String>,
}

/// Why a save wrote no CUE sheet although markers were expected.
enum NoCue {
    /// No markers remain anywhere, so nothing needs writing.
    Note(String),
    /// The destination keeps a CUE sheet this window did not write; it was
    /// preserved and its markers will resurface when the file is reopened.
    LeftInPlace(String),
}

type ExportResult = std::result::Result<ExportOutcome, String>;
type SaveMessage = (PathBuf, u64, ExportResult);
type SelectionMessage = (PathBuf, ExportResult);

struct Editor {
    window: Window,
    status: TextArea,
    activity: String,
    recovery: Option<String>,
    path: PathBuf,
    cue: Option<PathBuf>,
    destination: Option<PathBuf>,
    /// CUE sheets this window has written successfully, so
    /// an obsolete one can be removed and a foreign one never overwritten
    /// silently.
    saved_cues: Vec<(PathBuf, Vec<u8>)>,
    format: Info,
    edit: Edit,
    title_anchor: Option<usize>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    next_revision: u64,
    saved_revision: u64,
    playback: Option<Playback>,
    output: Option<audio::Output>,
    timeline: timeline_control::Timeline,
    /// Set by a scrub: the next refresh skips the status text rewrite, so a
    /// focused screen reader cannot announce its character changes.
    freeze_status: bool,
    device: Option<String>,
    speed: f32,
    scrub_step: usize,
    /// Loop segment scrub (/): a scrub move repeats a window one scrub step
    /// long, starting at the playhead, until it is stopped.
    scrub_loop: bool,
    /// Whether continuous playback continues from the playhead when a scrub
    /// snippet ends: Transport > Continue Playback After Scrubbing.
    resume_after_scrub: bool,
    announce_titles: bool,
    last_title: Option<usize>,
    device_items: Vec<(Option<String>, CheckMenuItem)>,
    speed_items: Vec<(f32, CheckMenuItem)>,
    session: Session,
    _open_document: OpenDocument,
    save: Option<Receiver<SaveMessage>>,
    save_selection: Option<Receiver<SelectionMessage>>,
    split: Option<Receiver<SplitMessage>>,
    insert: Option<Receiver<Insertion>>,
    analyze: Option<Receiver<Result<String>>>,
}

#[derive(Clone, Copy)]
enum Command {
    Open,
    Save,
    SaveAs,
    SaveSelection,
    SplitTracks,
    Analyze,
    Close,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Duplicate,
    InsertFile,
    Delete,
    Crop,
    SelectAll,
    Clear,
    Beginning,
    End,
    Move(i32, bool),
    StartMarker,
    EndMarker,
    GoStart,
    GoEnd,
    SelectionHome,
    SelectionEnd,
    SelectionStartBack,
    SelectionStartForward,
    SelectionEndBack,
    SelectionEndForward,
    PlayStop,
    PausePlay,
    SpeedUp,
    SpeedDown,
    ResetSpeed,
    ZoomIn,
    ZoomOut,
    Preview,
    ReportPosition,
    ReportSelection,
    JumpTo,
    PreviousTitle,
    NextTitle,
    GoToTitle,
    ReadTitles,
    AddTitle,
    RenameTitle,
    DeleteTitle,
    MoveNearestTitle,
    ShiftTitlesBack,
    ShiftTitlesForward,
    Status,
    Help,
}

pub fn choose_files(parent: Window, session: &Session) {
    let paths = FileDialog::new()
        .title("Open audio files")
        .filter("Audio", &["mp3", "mp2", "aac"])
        .open_many(Some(parent));
    for path in paths {
        open_file(parent, path, session);
    }
}

/// Open the audio files on the Explorer clipboard. A file manager copy puts
/// its file list on the clipboard as an HDROP; every supported file it holds
/// is opened the way the Open dialog would, and a clipboard without one is
/// reported. This is the blank window's paste: an editor's Ctrl+V pastes
/// audio frames and stays untouched.
pub fn paste_files_from_clipboard(parent: Window, session: &Session) {
    #[cfg(windows)]
    {
        let paths: Vec<PathBuf> = clipboard_file_paths()
            .into_iter()
            .filter(|path| is_supported_file(path))
            .collect();
        if paths.is_empty() {
            show_error(parent, "No MP3, MP2, or AAC file is on the clipboard.");
            return;
        }
        for path in paths {
            open_file(parent, path, session);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (parent, session);
    }
}

/// Whether a file is one this build can open.
fn is_supported_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("mp3" | "mp2" | "aac")
    )
}

/// The file paths of the clipboard's HDROP object, empty when the clipboard
/// holds no file list or cannot be read.
#[cfg(windows)]
fn clipboard_file_paths() -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    const CF_HDROP: u32 = 15;
    unsafe {
        if OpenClipboard(None).is_err() {
            return Vec::new();
        }
        let mut paths = Vec::new();
        if let Ok(handle) = GetClipboardData(CF_HDROP) {
            let drop = HDROP(handle.0);
            let count = DragQueryFileW(drop, u32::MAX, None);
            for index in 0..count {
                let len = DragQueryFileW(drop, index, None);
                if len > 0 {
                    let mut name = vec![0; len as usize + 1];
                    let read = DragQueryFileW(drop, index, Some(&mut name)) as usize;
                    if read > 0 && read < name.len() {
                        paths.push(PathBuf::from(std::ffi::OsString::from_wide(&name[..read])));
                    }
                }
            }
        }
        let _ = CloseClipboard();
        paths
    }
}

pub fn open_file(parent: Window, path: PathBuf, session: &Session) {
    let (sender, receiver) = mpsc::channel();
    let input = path.clone();
    let started = std::thread::Builder::new()
        .name("Audio import".into())
        .spawn(move || {
            let result = fs::read(&input)
                .map_err(Into::into)
                .and_then(|bytes| codec::open(&bytes))
                .map(|timeline| {
                    let cue_path = input.with_extension("cue");
                    let mut cue = None;
                    let mut cue_entries = Vec::new();
                    let mut cue_error = None;
                    match stream::read_cue(&cue_path) {
                        Ok(entries) => {
                            cue = Some(cue_path);
                            cue_entries = entries;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            cue_error =
                                Some(format!("Could not read {}: {error}", cue_path.display()));
                        }
                    }
                    Opened {
                        timeline,
                        cue,
                        cue_entries,
                        cue_error,
                    }
                });
            let _ = sender.send(result);
        });
    if let Err(error) = started {
        show_error(
            parent,
            &format!("Could not open {}. {error}", path.display()),
        );
        return;
    }
    let session = session.clone();
    let mut document = Some(session.opening_document());
    let timer = Rc::new(std::cell::Cell::new(None::<Timer>));
    let handle = Timer::every(50, {
        let timer = timer.clone();
        move || {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => Err("The import worker stopped.".into()),
            };
            if let Some(timer) = timer.take() {
                timer.stop();
            }
            match result {
                Ok(opened) => {
                    if let Some(document) = document.take() {
                        create(path.clone(), opened, session.clone(), document);
                    }
                }
                Err(error) => show_error(
                    parent,
                    &format!("Could not open {}. {error}", path.display()),
                ),
            }
        }
    });
    timer.set(Some(handle));
}

fn name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn same_path(left: &Path, right: &Path) -> bool {
    if let (Ok(left), Ok(right)) = (left.canonicalize(), right.canonicalize()) {
        return left == right;
    }
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn cue_message(outcome: &ExportOutcome) -> String {
    if let Some(cue) = &outcome.cue {
        format!(" CUE sheet: {}.", name(cue))
    } else if let Some(removed) = &outcome.removed_cue {
        format!(
            " Removed the obsolete CUE sheet {}, which this window wrote earlier.",
            name(removed)
        )
    } else if let Some(error) = &outcome.cue_error {
        format!(" The CUE sheet was not written. {error}")
    } else if let Some(note) = &outcome.cue_note {
        format!(" {note}")
    } else {
        String::new()
    }
}

fn saved_message(path: &Path, outcome: &ExportOutcome) -> String {
    format!("Saved {}.{}", name(path), cue_message(outcome))
}

/// Export the audio, then the CUE sheet when there are title markers. When
/// markers were expected but none remain, `no_cue_note` explains why nothing
/// was written, and `obsolete` — a CUE sheet this window wrote earlier at
/// this destination — is removed so stale markers cannot resurface.
fn export_with_cue(
    path: &Path,
    format: Info,
    frames: Frames,
    cue: Option<(PathBuf, Vec<(f64, String)>)>,
    no_cue: Option<NoCue>,
    obsolete: Option<(PathBuf, Vec<u8>)>,
    expected_cue: Option<Vec<u8>>,
) -> ExportResult {
    audio::export(path, format, frames).map_err(|error| error.to_string())?;
    let mut outcome = ExportOutcome {
        cue: None,
        written_cue: None,
        removed_cue: None,
        cue_error: None,
        cue_note: None,
    };
    if let Some((cue_path, entries)) = cue {
        match stream::write_editor_cue(&cue_path, &name(path), &entries, expected_cue.as_deref()) {
            Ok(contents) => {
                outcome.written_cue = Some((cue_path.clone(), contents));
                outcome.cue = Some(cue_path);
            }
            Err(error) => {
                outcome.cue_error =
                    Some(format!("Could not write {}: {error}", cue_path.display()));
            }
        }
    } else if let Some(no_cue) = no_cue {
        if let Some((removed, expected)) = obsolete.filter(|(removed, _)| removed.exists()) {
            match fs::read(&removed).and_then(|actual| {
                if actual == expected {
                    fs::remove_file(&removed)
                } else {
                    Err(std::io::Error::other(
                        "The existing CUE changed and was left in place.",
                    ))
                }
            }) {
                Ok(()) => outcome.removed_cue = Some(removed),
                Err(error) => {
                    outcome.cue_error = Some(format!(
                        "The old CUE sheet {} is obsolete but could not be removed: {error}",
                        removed.display()
                    ));
                }
            }
        }
        if outcome.removed_cue.is_none() && outcome.cue.is_none() && outcome.cue_error.is_none() {
            match no_cue {
                NoCue::Note(note) => outcome.cue_note = Some(note),
                NoCue::LeftInPlace(message) => outcome.cue_error = Some(message),
            }
        }
    }
    Ok(outcome)
}

/// The reviewable split report: one line per track with the file that was
/// written (or the failure), the source title, and the exact time range.
fn split_report_text(message: &SplitMessage, format: Info) -> String {
    let written = message
        .outcomes
        .iter()
        .filter(|outcome| outcome.result.is_ok())
        .count();
    let mut text = format!(
        "Split report\r\nSource: {}\r\nFolder: {}\r\n{} of {} file(s) written.\r\n",
        message.source.display(),
        message.folder.display(),
        written,
        message.outcomes.len(),
    );
    for outcome in &message.outcomes {
        text.push_str("\r\n");
        let range = format!(
            "{} to {}",
            time_text(format, outcome.range.start),
            time_text(format, outcome.range.end),
        );
        let track = format!("Title {}: {}.", outcome.number, title_name(&outcome.title),);
        match &outcome.result {
            Ok(path) => {
                text.push_str(&format!("{} - {track} {range}. Written.", name(path),));
                if path != &outcome.suggested {
                    text.push_str(&format!(
                        " The suggested name {} was already in use.",
                        name(&outcome.suggested),
                    ));
                }
            }
            Err(error) => text.push_str(&format!(
                "{} - {track} {range}. FAILED: {error}",
                name(&outcome.suggested),
            )),
        }
    }
    text.push_str("\r\n");
    text
}

pub(crate) fn show_error(window: Window, error: &str) {
    // Whatever held keyboard focus in this window before the box appeared
    // is asked for again afterwards; when nothing did (the box was opened
    // from a menu, for instance), the window itself is focused.
    let focus = focus::capture(window);
    message_box(
        Some(window),
        MessageKind::Error,
        Buttons::Ok,
        "DirectCut RS",
        error,
    );
    focus::restore(focus, &window);
}

/// The sections of the hotkeys document: what a group drives, then each
/// shortcut with its action. Help > Keyboard Shortcuts shows this list.
const HOTKEY_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Transport",
        &[
            (
                "Space",
                "Play, resume a pause, or stop and return to the playback start",
            ),
            (
                "Ctrl+Space",
                "Pause or resume; pausing leaves the playhead at the pause position",
            ),
            ("Shift+Space", "Preview the marked range and stop"),
            (
                "Shift+= or Shift+-",
                "Increase or decrease playback speed in 0.25x steps, from 0.25x to 4.00x",
            ),
            ("Ctrl+Shift+Backspace", "Reset playback speed to 1.00x"),
            (
                "Ctrl+J",
                "Jump to a time typed as ss, mm:ss, or hh:mm:ss, or to a CUE title typed as t24",
            ),
            (
                "Transport > Continue Playback After Scrubbing",
                "Whether playback continues from the playhead after a scrub ends (checked by default)",
            ),
        ],
    ),
    (
        "Scrubbing and the playhead",
        &[
            (
                "Left or Right",
                "Scrub by the current step, 1 frame by default; the blip starts at the playhead and runs a little past it",
            ),
            ("Ctrl+Left or Ctrl+Right", "Scrub about one second"),
            (
                "- or =",
                "Zoom the scrub step in (slower) or out (faster), from 1 frame to 1 second",
            ),
            (
                "/",
                "Toggle loop segment scrub: each scrub move repeats one scrub step from the playhead until stopped, and turning it off stops a sounding loop at its start",
            ),
            ("Alt+Home or Alt+End", "Go to the start or end marker"),
            (
                "Ctrl+Home or Ctrl+End",
                "Go to the start or end of the file",
            ),
            (
                "Ctrl+Shift+J",
                "Report the playhead position and current title",
            ),
        ],
    ),
    (
        "Markers and the selection",
        &[
            ("[ or ]", "Set the start or end marker at the playhead"),
            (
                "Ctrl+[ or Ctrl+]",
                "Move the start of the selection backward or forward one scrub step",
            ),
            (
                "Alt+[ or Alt+]",
                "Move the end of the selection backward or forward one scrub step",
            ),
            (
                "Home or End",
                "Go to the start or end of the selection; without one, a spoken message says so instead of moving",
            ),
            ("Escape", "Clear time selection (Edit menu)"),
            ("Ctrl+A", "Mark the entire timeline"),
            ("Ctrl+Shift+Space", "Report the time selection"),
        ],
    ),
    (
        "CUE titles",
        &[
            (
                "Ctrl+Shift+N or S",
                "Add a cue point at the playhead, the way S splits items in Reaper; the name may carry $file, which stands for the file the audio at the cue point came from",
            ),
            (
                "Ctrl+Shift+R",
                "Rename the cue point that contains the playhead",
            ),
            (
                "Ctrl+Shift+D",
                "Delete the cue point that contains the playhead",
            ),
            ("Ctrl+Shift+M", "Move the nearest cue point to the playhead"),
            (
                "Ctrl+Shift+B or Ctrl+Shift+F",
                "Shift every cue point back or forward one second",
            ),
            (
                "Ctrl+Shift+Left or Ctrl+Shift+Right",
                "Go to the previous or next CUE title",
            ),
            (
                "Ctrl+Shift+T",
                "Go to a CUE title by number, t number, or part of its name",
            ),
            (
                "Titles > Read Title List",
                "Show every CUE title start and derived length",
            ),
            (
                "Titles > Split Audio at Cue Points...",
                "Write each CUE title's audio as its own numbered file in a chosen folder",
            ),
            (
                "Titles > Announce Titles While Playing",
                "Speak each title as playback enters it",
            ),
        ],
    ),
    (
        "Editing",
        &[
            (
                "Ctrl+X, Ctrl+C, Ctrl+V",
                "Cut, copy, or paste audio; paste inserts at the playhead or replaces the marked range, asking first when that range is long",
            ),
            ("Ctrl+Z or Ctrl+Y", "Undo or redo"),
            ("Ctrl+D", "Duplicate the marked range after itself"),
            ("Insert", "Insert another audio file at the playhead"),
            ("Delete", "Delete the marked range and close the gap"),
            ("Edit > Crop to Selection", "Keep only the marked range"),
            (
                "File > Save Selection As...",
                "Export the marked range without changing the timeline",
            ),
        ],
    ),
    (
        "File and windows",
        &[
            ("Ctrl+O", "Open audio files"),
            ("Ctrl+S", "Save the edited audio"),
            ("Ctrl+Shift+S", "Choose a new output path"),
            ("Ctrl+I", "Analyze the source MP3 without modifying it"),
            ("Ctrl+W", "Close the window"),
            ("F6", "Open the timeline status"),
            ("F1", "Open the keyboard shortcuts message"),
            (
                "F1 or Help > Keyboard Shortcuts",
                "Show this list as a reviewable document with Save Report",
            ),
        ],
    ),
];

/// The hotkeys document: every shortcut with its action, grouped by what it
/// drives, as plain text a screen reader can browse line by line and the
/// report window can save.
fn hotkeys_document() -> String {
    let mut text = String::from(
        "DirectCut RS hotkeys\r\n\
         Ctrl+Shift+J reports the playhead, \
         and F6 opens the full timeline status. \
         Punctuation shortcuts such as [ and - name the key at the US-layout position; \
         on another keyboard layout, press the key that sits there.\r\n\r\n",
    );
    for &(section, entries) in HOTKEY_SECTIONS {
        text.push_str(section);
        text.push_str("\r\n\r\n");
        let width = entries
            .iter()
            .map(|(key, _)| key.len())
            .max()
            .unwrap_or(0)
            .min(34);
        for &(key, action) in entries {
            text.push_str(&format!("{key:<width$}  {action}\r\n"));
        }
        text.push_str("\r\n");
    }
    text
}

/// A read-only, selectable report window with a plain-text save command.
/// When it closes, keyboard focus goes back to `parent` — the window the
/// report was opened from — immediately and again from the event loop,
/// because closing a window hands activation around asynchronously.
fn show_report(
    parent: Window,
    title: &str,
    note_text: &str,
    save_title: &str,
    suggested: &str,
    text: String,
) {
    let window = Window::new(title);
    let column = VBox::new(window);
    column.set_padding(12);
    column.set_spacing(8);
    let note = Label::new(column, note_text);
    let area = TextArea::new(column);
    area.set_a11y_name("Report");
    area.set_read_only(true);
    area.set_min_size(760, 460);
    area.set_expand(1.0);
    area.set_text(&text);
    // The report view is selectable, not editable; Tab must keep cycling.
    editkeys::allow_tab_to_escape(&area);
    let buttons = HBox::new(column);
    buttons.set_spacing(8);
    let save = Button::new(buttons, "&Save Report...");
    let close = Button::new(buttons, "&Close");
    save.on_click({
        let text = text.clone();
        let suggested = suggested.to_string();
        let save_title = save_title.to_string();
        move || {
            let Some(path) = FileDialog::new()
                .title(&save_title)
                .filter("Text files", &["txt"])
                .file_name(&suggested)
                .save(Some(window))
            else {
                return;
            };
            match fs::write(&path, text.as_bytes()) {
                Ok(()) => note.set_text(&format!("Saved report to {}.", name(&path))),
                Err(error) => {
                    show_error(window, &format!("Could not save the report. {error}"));
                }
            }
        }
    });
    close.on_click(move || window.close());
    window.on_close(move || {
        focus::settle(&parent);
        true
    });
    window.set_size(820, 560);
    window.show();
    area.focus();
}

fn create(path: PathBuf, opened: Opened, session: Session, open_document: OpenDocument) {
    let Opened {
        timeline,
        cue,
        cue_entries,
        cue_error,
    } = opened;
    let window = Window::new("DirectCut RS");
    announce::attach(window);
    let column = VBox::new(window);
    column.set_padding(12);
    column.set_spacing(8);
    Label::new(column, "Timeline status (F6)");
    let strip = Label::new(column, "Timeline");
    strip.set_min_size(0, 24);
    let timeline_strip = timeline_control::Timeline::attach(&strip);
    let status = TextArea::new(column);
    status.set_a11y_name("Timeline status");
    status.set_read_only(true);
    status.set_min_size(480, 125);
    // The status display is not a tab stop: keyboard focus cannot park in it,
    // so its per-scrub text changes are not announced character by character.
    // F6 remains the full readable status.
    editkeys::make_display_only(&status);
    // The read-only status view must not swallow Tab either.
    editkeys::allow_tab_to_escape(&status);
    let bar = MenuBar::new(window);
    let recovery = report::summary(&timeline);
    let format = timeline.info;
    let frames = timeline.frames;
    let titles = cue_titles(cue_entries, format, frames.len());
    let name = format.name();
    let activity = if let Some(error) = cue_error {
        format!("Opened {name}, but its CUE sheet could not be read. {error}")
    } else if cue.is_some() {
        format!("Opened {name} with {} CUE title(s).", titles.len())
    } else {
        format!("Opened {name}.").into()
    };
    let source = source_identity(&path);
    let playback_defaults = session.settings.borrow().playback.clone();
    let editor = Rc::new(RefCell::new(Editor {
        window,
        status,
        activity,
        recovery,
        path,
        cue,
        destination: None,
        saved_cues: Vec::new(),
        format,
        edit: Edit {
            frames,
            titles,
            sources: vec![(0, source)],
            cursor: 0,
            start: None,
            end: None,
            revision: 0,
        },
        title_anchor: None,
        undo: Vec::new(),
        redo: Vec::new(),
        next_revision: 1,
        saved_revision: 0,
        playback: None,
        output: None,
        timeline: timeline_strip,
        freeze_status: false,
        device: playback_defaults.device,
        speed: playback_defaults.speed,
        scrub_step: playback_defaults.scrub_step.max(1),
        scrub_loop: playback_defaults.scrub_loop,
        resume_after_scrub: playback_defaults.resume_after_scrub,
        announce_titles: false,
        last_title: None,
        device_items: Vec::new(),
        speed_items: Vec::new(),
        session,
        _open_document: open_document,
        save: None,
        save_selection: None,
        split: None,
        insert: None,
        analyze: None,
    }));
    let timer = Timer::every(75, {
        let editor = Rc::downgrade(&editor);
        move || {
            if let Some(editor) = editor.upgrade()
                && let Ok(mut editor) = editor.try_borrow_mut()
            {
                editor.poll();
            }
        }
    });
    use Command::*;
    let add = |menu: Menu, label: &str, shortcut: &str, command: Command| {
        let item = MenuItem::new(menu, label);
        if !shortcut.is_empty() {
            item.set_accel(shortcut);
        }
        let editor = editor.clone();
        item.on_click(move || match command {
            Close => {
                if can_close(&editor) {
                    timer.stop();
                    window.close();
                }
            }
            Open => {
                let session = editor.borrow().session.clone();
                choose_files(window, &session);
            }
            _ => {
                let result = editor.borrow_mut().run(command);
                if let Err(error) = result {
                    show_error(window, &error.to_string());
                }
            }
        });
    };
    let file = Menu::new(bar, "&File");
    for &(label, shortcut, command) in &[
        ("&Open Audio...", "Ctrl+O", Open),
        ("&Save Edited Audio", "Ctrl+S", Save),
        ("Save Edited Audio &As...", "Ctrl+Shift+S", SaveAs),
        ("Save S&election As...", "", SaveSelection),
        ("Ana&lyze Source MP3...", "Ctrl+I", Analyze),
        ("&Close", "Ctrl+W", Close),
    ][..]
    {
        add(file, label, shortcut, command);
    }
    let edit = Menu::new(bar, "&Edit");
    for &(label, shortcut, command) in &[
        ("&Undo", "Ctrl+Z", Undo),
        ("&Redo", "Ctrl+Y", Redo),
        ("Cu&t", "Ctrl+X", Cut),
        ("&Copy", "Ctrl+C", Copy),
        ("&Paste", "Ctrl+V", Paste),
        ("Duplicate S&election", "Ctrl+D", Duplicate),
        ("&Insert Audio at Playhead...", "Insert", InsertFile),
        ("&Delete Selection", "Del", Delete),
        ("Crop to &Selection", "", Crop),
        ("Select &All", "Ctrl+A", SelectAll),
        ("Clear Time Se&lection", "Esc", Clear),
    ][..]
    {
        add(edit, label, shortcut, command);
    }
    let navigate = Menu::new(bar, "&Navigate");
    for &(label, shortcut, command) in &[
        ("File &Start", "Ctrl+Home", Beginning),
        ("File &End", "Ctrl+End", End),
        ("&Previous Frame", "Left", Move(-1, false)),
        ("&Next Frame", "Right", Move(1, false)),
        ("&Back One Second", "Ctrl+Left", Move(-1, true)),
        ("&Forward One Second", "Ctrl+Right", Move(1, true)),
        ("Scrub Zoom &In", "-", ZoomIn),
        ("Scrub Zoom &Out", "=", ZoomOut),
        ("Repo&rt Position", "Ctrl+Shift+J", ReportPosition),
    ][..]
    {
        add(navigate, label, shortcut, command);
    }
    let scrub_loop_item = CheckMenuItem::new(navigate, "&Loop Segment Scrub");
    scrub_loop_item.set_checked(editor.borrow().scrub_loop);
    {
        let editor = editor.clone();
        scrub_loop_item.on_toggle(move |checked| {
            if let Err(error) = editor.borrow_mut().set_scrub_loop(checked) {
                show_error(window, &error.to_string());
            }
        });
    }
    scrub_loop_item.set_accel("/");
    let markers = Menu::new(bar, "&Markers");
    for &(label, shortcut, command) in &[
        ("Set Start &Marker", "[", StartMarker),
        ("Se&t End Marker", "]", EndMarker),
        (
            "Move Start of Selection &Backward",
            "Ctrl+[",
            SelectionStartBack,
        ),
        (
            "Move Start of Selection Forwar&d",
            "Ctrl+]",
            SelectionStartForward,
        ),
        ("Move End of Selection Back&ward", "Alt+[", SelectionEndBack),
        (
            "Move End of Selection &Forward",
            "Alt+]",
            SelectionEndForward,
        ),
        ("&Go to Start Marker", "Alt+Home", GoStart),
        ("Go to End Mar&ker", "Alt+End", GoEnd),
        ("Selection Start", "Home", SelectionHome),
        ("Selection End", "End", SelectionEnd),
        (
            "Report Time Sele&ction",
            "Ctrl+Shift+Space",
            ReportSelection,
        ),
    ][..]
    {
        add(markers, label, shortcut, command);
    }
    let titles = Menu::new(bar, "&Titles");
    for &(label, shortcut, command) in &[
        ("&Previous Title", "Ctrl+Shift+Left", PreviousTitle),
        ("&Next Title", "Ctrl+Shift+Right", NextTitle),
        ("&Go to Title...", "Ctrl+Shift+T", GoToTitle),
        ("&Read Title List", "", ReadTitles),
        ("&Split Audio at Cue Points...", "", SplitTracks),
    ][..]
    {
        add(titles, label, shortcut, command);
    }
    MenuSeparator::new(titles);
    let cue_point = Menu::new(titles, "C&ue Point");
    for &(label, shortcut, command) in &[
        ("&New Cue Point at Playhead", "Ctrl+Shift+N", AddTitle),
        ("Add Cue Point at &Cursor", "S", AddTitle),
        ("&Rename Cue Point", "Ctrl+Shift+R", RenameTitle),
        ("&Delete Cue Point", "Ctrl+Shift+D", DeleteTitle),
        (
            "&Move Nearest Cue Point to Playhead",
            "Ctrl+Shift+M",
            MoveNearestTitle,
        ),
        (
            "Shift Cue Points &Back One Second",
            "Ctrl+Shift+B",
            ShiftTitlesBack,
        ),
        (
            "Shift Cue Points &Forward One Second",
            "Ctrl+Shift+F",
            ShiftTitlesForward,
        ),
    ][..]
    {
        add(cue_point, label, shortcut, command);
    }
    MenuSeparator::new(titles);
    let announce_titles = CheckMenuItem::new(titles, "&Announce Titles While Playing");
    {
        let editor = editor.clone();
        announce_titles.on_toggle(move |checked| {
            let mut editor = editor.borrow_mut();
            editor.announce_titles = checked;
            editor.last_title = None;
        });
    }
    let transport = Menu::new(bar, "Trans&port");
    add(transport, "Play / &Stop", "Space", PlayStop);
    add(transport, "&Pause / Resume", "Ctrl+Space", PausePlay);
    add(transport, "P&review Selection", "Shift+Space", Preview);
    let resume_item = CheckMenuItem::new(transport, "&Continue Playback After Scrubbing");
    resume_item.set_checked(editor.borrow().resume_after_scrub);
    {
        let editor = editor.clone();
        resume_item.on_toggle(move |checked| {
            let mut editor = editor.borrow_mut();
            editor.resume_after_scrub = checked;
            editor.store_settings(|saved, editor| {
                saved.resume_after_scrub = editor.resume_after_scrub
            });
            editor.activity = if checked {
                "Playback will continue from the playhead after a scrub ends.".into()
            } else {
                "Playback will stay stopped after a scrub ends.".into()
            };
            editor.speak(&editor.activity);
            editor.refresh();
        });
    }
    MenuSeparator::new(transport);
    add(transport, "&Jump to Time...", "Ctrl+J", JumpTo);
    MenuSeparator::new(transport);

    let device_menu = Menu::new(transport, "Playback &Device");
    let mut device_items = Vec::new();
    let default_device = CheckMenuItem::new(device_menu, "System &Default");
    default_device.set_checked(editor.borrow().device.is_none());
    {
        let editor = editor.clone();
        default_device.on_toggle(move |checked| {
            if checked {
                if let Err(error) = editor.borrow_mut().select_device(None) {
                    show_error(window, &error.to_string());
                }
            } else {
                let checked = editor.borrow().device.is_none();
                default_device.set_checked(checked);
            }
        });
    }
    device_items.push((None, default_device));
    for name in audio::output_devices() {
        let item = CheckMenuItem::new(device_menu, &name.replace('&', "&&"));
        item.set_checked(editor.borrow().device.as_deref() == Some(name.as_str()));
        {
            let editor = editor.clone();
            let name = name.clone();
            item.on_toggle(move |checked| {
                if checked {
                    if let Err(error) = editor.borrow_mut().select_device(Some(name.clone())) {
                        show_error(window, &error.to_string());
                    }
                } else {
                    let checked = editor.borrow().device.as_deref() == Some(name.as_str());
                    item.set_checked(checked);
                }
            });
        }
        device_items.push((Some(name), item));
    }

    let speed_menu = Menu::new(transport, "Playback Sp&eed");
    add(speed_menu, "&Increase Speed", "Shift+=", SpeedUp);
    add(speed_menu, "&Decrease Speed", "Shift+-", SpeedDown);
    add(
        speed_menu,
        "&Reset Speed",
        "Ctrl+Shift+Backspace",
        ResetSpeed,
    );
    MenuSeparator::new(speed_menu);
    let mut speed_items = Vec::new();
    for speed in speed_steps() {
        let item = CheckMenuItem::new(speed_menu, &format!("{speed:.2}x"));
        item.set_checked((editor.borrow().speed - speed).abs() < 0.001);
        {
            let editor = editor.clone();
            item.on_toggle(move |checked| {
                if checked {
                    editor.borrow_mut().set_speed(speed);
                } else {
                    item.set_checked((editor.borrow().speed - speed).abs() < 0.001);
                }
            });
        }
        speed_items.push((speed, item));
    }
    MenuSeparator::new(transport);
    add(transport, "Read Timeline St&atus", "F6", Status);
    let help_menu = Menu::new(bar, "&Help");
    add(help_menu, "&Keyboard Shortcuts", "F1", Help);
    {
        let mut editor = editor.borrow_mut();
        editor.device_items = device_items;
        editor.speed_items = speed_items;
    }
    window.on_close({
        let editor = editor.clone();
        move || {
            let close = can_close(&editor);
            if close {
                timer.stop();
            }
            close
        }
    });
    editor.borrow_mut().refresh();
    window.set_size(570, 230);
    window.show();
    status.focus();
}

fn can_close(editor: &RefCell<Editor>) -> bool {
    let (window, busy, message, dirty) = {
        let editor = editor.borrow();
        let message = if editor.insert.is_some() {
            "Audio is being read for insertion. Close this window after it finishes."
        } else {
            "Audio is being saved. Close this window after it finishes."
        };
        (
            editor.window,
            editor.save.is_some()
                || editor.save_selection.is_some()
                || editor.split.is_some()
                || editor.insert.is_some(),
            message,
            editor.dirty(),
        )
    };
    if busy {
        show_error(window, message);
        return false;
    }
    !dirty || {
        let focus = focus::capture(window);
        let answer = message_box(
            Some(window),
            MessageKind::Warning,
            Buttons::OkCancel,
            "Discard unsaved edits?",
            "Close this window and discard unsaved edits?",
        );
        focus::restore(focus, &window);
        answer == Answer::Ok
    }
}

impl Editor {
    fn dirty(&self) -> bool {
        self.edit.revision != self.saved_revision
    }

    fn position(&self) -> usize {
        self.playback
            .as_ref()
            .map_or(self.edit.cursor, |p| p.position(self.format))
            .min(self.edit.frames.len())
    }

    fn status_text(&self) -> String {
        let time = |frame| time_text(self.format, frame);
        let marker = |frame: Option<usize>| frame.map_or("unset".into(), time);
        let state = match &self.playback {
            Some(p) if p.sink.is_paused() => "Paused",
            Some(p) if p.looped() => "Scrub loop",
            Some(_) => "Playing",
            None => "Stopped",
        };
        let looping = self.playback.as_ref().is_some_and(|p| p.looped());
        let transport = if looping || (self.speed - 1.0).abs() < 0.001 {
            state.to_string()
        } else {
            format!("{state} at {:.2}x speed", self.speed)
        };
        let titles = if self.edit.titles.is_empty() {
            "No CUE titles.".to_string()
        } else {
            match self.title_at(self.position()) {
                Some(index) => {
                    let marker = &self.edit.titles[index];
                    let end = self
                        .edit
                        .titles
                        .get(index + 1)
                        .map_or(self.edit.frames.len(), |next| next.frame);
                    format!(
                        "Title {} of {}: {}. Length {}.",
                        index + 1,
                        self.edit.titles.len(),
                        expanded_title(&marker.title, &self.edit.sources, marker.frame),
                        time(end.saturating_sub(marker.frame))
                    )
                }
                None => format!("Before title 1 of {}.", self.edit.titles.len()),
            }
        };
        let capabilities = self.format.disclosure();
        // The average transport bitrate, so a mixed-rate timeline can be
        // checked on the file itself; a VBR stream shows its average.
        let bitrate = codec::average_kbps(self.format, &self.edit.frames)
            .map(|kbps| format!(", about {:.0} kbps", kbps))
            .unwrap_or_default();
        format!(
            "{transport}. Position {} of {}. Scrub step {}.\r\nStart marker {}. End marker {}.\r\n{titles}\r\n{capabilities}\r\n{} Hz, {} {}{}. One timeline: paste inserts at the playhead and Delete closes the gap. {}\r\n{}{}{}",
            time(self.position()),
            time(self.edit.frames.len()),
            scrub_text(self.scrub_step, self.format),
            marker(self.edit.start),
            marker(self.edit.end),
            self.format.rate(),
            self.format.channels(),
            if self.format.channels() == 1 {
                "channel"
            } else {
                "channels"
            },
            bitrate,
            if self.dirty() { "Unsaved edits." } else { "" },
            self.activity,
            if self.recovery.is_some() { "\r\n" } else { "" },
            self.recovery.as_deref().unwrap_or(""),
        )
    }

    fn speak(&self, text: &str) {
        announce::announce(self.window, text);
    }

    fn refresh(&mut self) {
        self.refresh_with(false);
    }

    /// Push the timeline facts and, normally, the status text and window
    /// title. `during_playback` marks the per-tick playback refresh: while
    /// a scrub is sounding (`freeze_status` was set by the scrub), the text
    /// stays untouched and the freeze stays set, so the strip's playhead
    /// moves without a focused screen reader announcing character changes.
    /// The first refresh after the scrub ends — a command, or the snippet's
    /// end — skips the rewrite once and clears the freeze, exactly as this
    /// behaved before the per-tick refresh existed.
    fn refresh_with(&mut self, during_playback: bool) {
        self.timeline.update(
            self.edit.frames.len(),
            self.position(),
            self.edit.start,
            self.edit.end,
            self.edit.titles.iter().map(|marker| marker.frame).collect(),
        );
        if self.freeze_status {
            // A scrub moved the playhead: the strip shows the new position,
            // and rewriting the status text here made a focused screen
            // reader announce its character changes per keypress. The text
            // refreshes on the next command or when the snippet ends, and
            // Ctrl+Shift+J reports the position on demand.
            if !during_playback {
                self.freeze_status = false;
            }
        } else if !during_playback || !crate::focus::has_focus(&self.status) {
            let status = self.status_text();
            if self.status.text() != status {
                self.status.set_text(&status);
            }
        }
        let title = format!(
            "{}{} - DirectCut RS",
            name(self.destination.as_deref().unwrap_or(&self.path)),
            if self.dirty() { " *" } else { "" },
        );
        if title != self.window.title() {
            self.window.set_title(&title);
        }
    }

    fn poll(&mut self) {
        let ended = self.playback.as_ref().is_some_and(|p| p.sink.empty());
        if ended {
            let position = self.position();
            let (error, snippet, resume) = {
                let p = self.playback.as_ref().unwrap();
                (p.error.lock().unwrap().take(), p.snippet, p.resume)
            };
            self.playback = None;
            if let Some(error) = error {
                self.edit.cursor = position;
                self.activity = format!("Playback stopped. {error}");
            } else if !snippet {
                // Normal playback ran to the end; bounded playback such as a
                // scrub snippet or a selection preview keeps the playhead.
                self.edit.cursor = self.edit.frames.len();
            } else if resume {
                // A scrub interrupted continuous playback: resume it from the
                // playhead the scrub left behind. If resuming fails, report it
                // and leave the transport stopped.
                if let Err(error) = self.play() {
                    self.activity = format!("Playback did not resume. {error}");
                }
            }
            self.refresh();
        }
        if self.announce_titles
            && self
                .playback
                .as_ref()
                .is_some_and(|p| !p.snippet && !p.sink.is_paused())
        {
            let position = self.position();
            let index = title_at_frame(&self.edit.titles, position);
            if index != self.last_title {
                self.last_title = index;
                if let Some(index) = index {
                    let marker = &self.edit.titles[index];
                    let message = format!(
                        "Title {} of {}: {}.",
                        index + 1,
                        self.edit.titles.len(),
                        expanded_title(&marker.title, &self.edit.sources, marker.frame)
                    );
                    self.activity = message.clone();
                    self.speak(&message);
                }
            }
        } else if self.playback.is_none() {
            self.last_title = None;
        }
        if let Some(save) = &self.save {
            match save.try_recv() {
                Ok((path, revision, result)) => {
                    self.save = None;
                    match result {
                        Ok(outcome) => {
                            let message = saved_message(&path, &outcome);
                            self.activity = message.clone();
                            self.speak(&message);
                            self.destination = Some(path);
                            // A CUE write failure is not a successful save:
                            // title markers may exist only in memory, so the
                            // document stays dirty and closing still asks.
                            if outcome.cue_error.is_none() {
                                self.saved_revision = revision;
                            }
                            self.remember_cue_outcome(&outcome);
                        }
                        Err(error) => {
                            self.activity = format!("Save failed. {error}");
                            let window = self.window;
                            let message = self.activity.clone();
                            App::post(move || {
                                if window.is_alive() {
                                    show_error(window, &message);
                                }
                            });
                        }
                    }
                    self.refresh();
                }
                Err(TryRecvError::Disconnected) => {
                    self.save = None;
                    self.activity = "Save failed. The export worker stopped.".into();
                    self.refresh();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(selection) = &self.save_selection {
            match selection.try_recv() {
                Ok((path, result)) => {
                    self.save_selection = None;
                    match result {
                        Ok(outcome) => {
                            let message = format!(
                                "Saved the selection to {}.{}",
                                name(&path),
                                cue_message(&outcome)
                            );
                            self.activity = message.clone();
                            self.speak(&message);
                            self.remember_cue_outcome(&outcome);
                        }
                        Err(error) => {
                            self.activity = format!("Selection save failed. {error}");
                            let window = self.window;
                            let message = self.activity.clone();
                            App::post(move || {
                                if window.is_alive() {
                                    show_error(window, &message);
                                }
                            });
                        }
                    }
                    self.refresh();
                }
                Err(TryRecvError::Disconnected) => {
                    self.save_selection = None;
                    self.activity = "Selection save failed. The export worker stopped.".into();
                    self.refresh();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(split) = &self.split {
            match split.try_recv() {
                Ok(message) => {
                    self.split = None;
                    let written = message
                        .outcomes
                        .iter()
                        .filter(|outcome| outcome.result.is_ok())
                        .count();
                    self.activity = if written == message.outcomes.len() {
                        format!(
                            "Split into {written} file(s) in {}. The report window lists each one.",
                            message.folder.display()
                        )
                    } else {
                        format!(
                            "Split wrote {written} of {} file(s); the report window lists what happened.",
                            message.outcomes.len()
                        )
                    };
                    self.speak(&self.activity);
                    let report = split_report_text(&message, self.format);
                    let title = format!("Split report - {}", name(&message.source));
                    show_report(
                        self.window,
                        &title,
                        "Every split output with its source title and exact time range. Files that could not be written show why. Save Report writes this list as a text file.",
                        "Save split report",
                        &report::suggested_name(&message.source, "split"),
                        report,
                    );
                }
                Err(TryRecvError::Disconnected) => {
                    self.split = None;
                    self.activity = "Split failed. The export worker stopped.".into();
                }
                Err(TryRecvError::Empty) => {}
            }
            self.refresh();
        }
        if let Some(insert) = &self.insert {
            match insert.try_recv() {
                Ok(insertion) => {
                    self.insert = None;
                    // An edit while the file was being read moved the
                    // timeline; the captured position no longer means what it
                    // did, so the insertion is refused rather than landing
                    // somewhere else.
                    if insertion.revision != self.edit.revision {
                        self.activity = format!(
                            "The timeline changed while {} was being read, so the insertion was cancelled. Try again.",
                            name(&insertion.path)
                        );
                        let window = self.window;
                        let message = self.activity.clone();
                        App::post(move || {
                            if window.is_alive() {
                                show_error(window, &message);
                            }
                        });
                    } else {
                        match insertion.result {
                            Ok(timeline) => {
                                if let Err(error) = check_splice_format(self.format, timeline.info)
                                {
                                    self.activity = format!(
                                        "Could not insert {}. {error}",
                                        name(&insertion.path)
                                    );
                                    let window = self.window;
                                    let message = self.activity.clone();
                                    App::post(move || {
                                        if window.is_alive() {
                                            show_error(window, &message);
                                        }
                                    });
                                } else if self.mixing_bitrates(&timeline.frames) {
                                    // The user read the bitrate warning and
                                    // chose not to combine. Nothing changed.
                                    self.activity = "The insertion was cancelled.".into();
                                    let message = self.activity.clone();
                                    self.speak(&message);
                                } else {
                                    let count = timeline.frames.len();
                                    let gaps = timeline.has_gaps();
                                    let position = insertion.position.min(self.edit.frames.len());
                                    let file = source_identity(&insertion.path);
                                    let same_before = source_at(&self.edit.sources, position)
                                        .is_some_and(|source| source == file);
                                    self.replace(position..position, timeline.frames, &[(0, file)]);
                                    let mut lead = format!(
                                        "Inserted {} frames ({}) from {} at {}. The timeline is now {} long.",
                                        count,
                                        time_text(self.format, count),
                                        name(&insertion.path),
                                        time_text(self.format, position),
                                        time_text(self.format, self.edit.frames.len()),
                                    );
                                    if gaps {
                                        lead.push_str(" The source had gaps; retained frames were inserted without replacement audio.");
                                    }
                                    self.activity = self.after_combine(position, same_before, lead);
                                    let message = self.activity.clone();
                                    self.speak(&message);
                                }
                            }
                            Err(error) => {
                                self.activity =
                                    format!("Could not insert {}. {error}", name(&insertion.path));
                                let window = self.window;
                                let message = self.activity.clone();
                                App::post(move || {
                                    if window.is_alive() {
                                        show_error(window, &message);
                                    }
                                });
                            }
                        }
                    }
                    self.refresh();
                }
                Err(TryRecvError::Disconnected) => {
                    self.insert = None;
                    self.activity = "Insert failed. The import worker stopped.".into();
                    self.refresh();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(analysis) = &self.analyze {
            match analysis.try_recv() {
                Ok(result) => {
                    self.analyze = None;
                    match result {
                        Ok(text) => {
                            self.activity =
                                "Analysis complete. The source file was not modified.".into();
                            let title = format!("Analysis - {}", name(&self.path));
                            show_report(
                                self.window,
                                &title,
                                "Read-only analysis. The source file was not modified.",
                                "Save analysis report",
                                &report::suggested_name(&self.path, "analysis"),
                                text,
                            );
                        }
                        Err(error) => {
                            self.activity = format!("Analysis failed. {error}");
                            let window = self.window;
                            let message = self.activity.clone();
                            App::post(move || {
                                if window.is_alive() {
                                    show_error(window, &message);
                                }
                            });
                        }
                    }
                    self.refresh();
                }
                Err(TryRecvError::Disconnected) => {
                    self.analyze = None;
                    self.activity = "Analysis failed. The analysis worker stopped.".into();
                    self.refresh();
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        // While anything is playing, the strip's playhead and the status
        // line track it, so the current time is on the status bar without a
        // command, and a title announcement shows as it is spoken. A
        // sounding scrub keeps holding the status text (the freeze the
        // scrub set), exactly as before.
        if self.playback.is_some() {
            self.refresh_with(true);
        }
    }

    fn stop_here(&mut self) {
        self.edit.cursor = self.position();
        self.playback = None;
    }

    /// Run `dialog` — any modal shown over this window — and re-establish
    /// keyboard focus where the window had it afterwards. When nothing in
    /// the window held focus, which is the normal case for a command from
    /// a menu, the window itself is focused; its keys are accelerators and
    /// work either way, and the screen reader is left on a quiet object
    /// rather than inside a text field.
    fn modal<R>(&self, dialog: impl FnOnce() -> R) -> R {
        let focus = focus::capture(self.window);
        let result = dialog();
        focus::restore(focus, &self.window);
        result
    }

    /// Whether combining `incoming` with the timeline would mix MP3
    /// transport bitrates, and the user chose not to after the warning.
    /// Insert and paste require the same format, so both sides are the same
    /// kind of stream; the comparison is the average bitrate at whole-kbps
    /// precision, and combining stays available on Yes because the result
    /// is not re-encoded either way.
    fn mixing_bitrates(&mut self, incoming: &Frames) -> bool {
        if self.format.kind() != codec::Kind::Mp3 {
            return false;
        }
        let current = codec::average_kbps(self.format, &self.edit.frames);
        let incoming = codec::average_kbps(self.format, incoming);
        if !bitrates_differ(current, incoming) {
            return false;
        }
        let answer = self.modal(|| {
            message_box(
                Some(self.window),
                MessageKind::Warning,
                Buttons::YesNo,
                "Combine different bitrates?",
                &format!(
                    "The open file is about {} and the audio being added is about {}.\r\n\r\nThe export may use variable bitrate (VBR) without re-encoding. Check whether your destination requires constant bitrate (CBR). Combine them anyway?",
                    kbps_text(current),
                    kbps_text(incoming),
                ),
            )
        });
        answer != Answer::Yes
    }

    /// Whether a paste that would replace the marked range should stop and
    /// ask first. See [`REPLACE_ASK_SECONDS`] and [`replace_needs_confirm`]
    /// for the line between asked and ordinary.
    fn replace_declined(&self, range: &std::ops::Range<usize>, incoming: usize) -> bool {
        if !replace_needs_confirm(range.len(), self.format) {
            return false;
        }
        if !self.session.settings.borrow().playback.confirm_long_paste {
            return false;
        }
        let message = format!(
            "Replace the marked range from {} to {} ({} of audio) with {} of clipboard audio?",
            time_text(self.format, range.start),
            time_text(self.format, range.end),
            time_text(self.format, range.len()),
            time_text(self.format, incoming)
        );
        let (confirmed, remember) =
            self.modal(|| crate::dialogs::confirm_paste(self.window, &message));
        if confirmed && remember {
            self.store_settings(|saved, _| saved.confirm_long_paste = false);
        }
        !confirmed
    }

    /// The consolidated result of a successful paste or insert: `lead`
    /// states what was added and where, and — when the timeline already has
    /// cue points, none sits exactly at the boundary, and the added audio
    /// came from a different file than what was there — the answer to
    /// whether a cue point should mark the file boundary. Combining dialog
    /// takes into a file without a sheet asks nothing, and the boundary of
    /// audio pasted at the very end has no track to name.
    fn after_combine(&mut self, boundary: usize, same_before: bool, lead: String) -> String {
        let total = self.edit.frames.len();
        let mut message = lead;
        if !same_before && boundary_ask_wanted(&self.edit.titles, boundary, total) {
            let answer = self.modal(|| {
                message_box(
                    Some(self.window),
                    MessageKind::Question,
                    Buttons::YesNo,
                    "Add a cue point at the file boundary?",
                    &format!(
                        "The boundary is at {}. A cue point there is named $file, which always shows the file the audio at it came from, so splitting or saving can mark where the pasted file begins. Add it?",
                        time_text(self.format, boundary),
                    ),
                )
            });
            if answer == Answer::Yes {
                self.record_title_edit();
                let index = insert_title(&mut self.edit.titles, boundary, FILE_TOKEN);
                let marker = &self.edit.titles[index];
                message.push_str(&format!(
                    " Cue point {} ({}) added at the boundary.",
                    index + 1,
                    expanded_title(&marker.title, &self.edit.sources, marker.frame),
                ));
            } else {
                message.push_str(" No cue point added at the boundary.");
            }
        }
        message
    }

    fn seek(&mut self, position: usize) -> Result<()> {
        // Any explicit seek cancels a title navigation anchor. Title jumps set
        // a new anchor after seeking.
        self.title_anchor = None;
        let playing = self.playback.as_ref().is_some_and(|p| !p.sink.is_paused());
        self.playback = None;
        self.edit.cursor = position.min(self.edit.frames.len());
        if playing && self.edit.cursor < self.edit.frames.len() {
            self.play()?;
        }
        Ok(())
    }

    /// Toggle loop segment scrub. While it is on, a scrub move repeats a
    /// window one scrub step long, starting at the playhead, until it is
    /// stopped — the zoom keys set that window's length. Turning it on while
    /// the transport is stopped starts a loop at once, so the change is
    /// audible immediately; turning it off stops a sounding loop and leaves
    /// the cursor at the loop's start.
    fn set_scrub_loop(&mut self, on: bool) -> Result<()> {
        if self.scrub_loop != on {
            self.scrub_loop = on;
            self.store_settings(|saved, editor| saved.scrub_loop = editor.scrub_loop.clone());
            self.activity = if on {
                "Loop segment scrub on. Each scrub move repeats one scrub step from the playhead until you stop it.".into()
            } else {
                "Loop segment scrub off. Scrub moves play a one-shot snippet again.".into()
            };
            self.speak(&self.activity);
            if on {
                if self.playback.is_none() && self.edit.cursor < self.edit.frames.len() {
                    self.scrub(self.position())?;
                }
            } else if let Some(p) = &self.playback {
                // Turning the mode off stops a loop that is sounding and
                // leaves the cursor at its start. A paused loop keeps its
                // parked position, and continuous playback or a snippet
                // keeps going.
                if p.looped() && !p.sink.is_paused() {
                    let start = p.start;
                    self.playback = None;
                    self.edit.cursor = start;
                }
            }
            self.freeze_status = false;
        }
        self.refresh();
        Ok(())
    }

    /// Move the playhead and play a short snippet from the new position, the
    /// way a tape scrub sounds. The snippet begins exactly at the playhead and
    /// runs about a tenth of a second past it, at normal speed whatever the
    /// speed setting is, and the playhead stays where the move put it when the
    /// snippet ends. Continuous playback interrupted by a scrub resumes from
    /// the playhead once the snippet ends; a paused or stopped transport
    /// stays put, so a stopped scrub only sounds the snippet. With loop
    /// segment scrub on, the move instead repeats a window one scrub step
    /// long from the playhead until it is stopped.
    fn scrub(&mut self, position: usize) -> Result<()> {
        let total = self.edit.frames.len();
        let position = position.min(total);
        // A scrub that interrupts continuous playback — or an earlier scrub
        // already waiting to resume it — still counts as playing. A paused
        // or stopped transport, a selection preview, or a plain snippet does
        // not. Loop segment scrub never schedules a resume: the loop runs
        // until it is stopped, and Transport > Continue Playback After
        // Scrubbing can turn the resume off altogether.
        let resume = self.resume_after_scrub
            && !self.scrub_loop
            && self
                .playback
                .as_ref()
                .is_some_and(|p| p.resume || (!p.snippet && !p.sink.is_paused()));
        self.playback = None;
        self.edit.cursor = position;
        self.title_anchor = None;
        if position >= total {
            return Ok(());
        }
        self.ensure_output()?;
        let info = self.format;
        let frames = self.edit.frames.clone();
        let loop_scrub = self.scrub_loop;
        let step = self.scrub_step.min(total - position);
        let snap = scrub_snap_frames(self.format);
        let started = self.start_playback(|output| {
            if loop_scrub {
                // Repeat a window exactly one scrub step long, starting at
                // the playhead: the zoom keys set the window, so an exact
                // start is found by nudging until the wanted sound sits at
                // the beginning of the repeating window.
                output.start_loop(info, &frames, position, step)
            } else {
                output.start(info, &frames, position, 1.0, Some(snap))
            }
        });
        match started {
            Ok(mut scrub) => {
                scrub.resume = resume;
                self.playback = Some(scrub);
                self.freeze_status = true;
            }
            Err(error) => {
                self.playback = None;
                self.activity = format!(
                    "Scrubbing from {} produced no sound. {error}",
                    time_text(self.format, position)
                );
            }
        }
        Ok(())
    }

    /// Play exactly the marked range at the current speed, then leave the
    /// playhead at the range start for a replay or an edit.
    fn preview_selection(&mut self) -> Result<()> {
        let range = self.edit.selection()?;
        let end = range.end.min(self.edit.frames.len());
        let start = range.start.min(end);
        if start >= self.edit.frames.len() {
            return Err("The selection is past the end of the timeline.".into());
        }
        self.playback = None;
        self.edit.cursor = start;
        self.title_anchor = None;
        let info = self.format;
        let frames = self.edit.frames.clone();
        let speed = self.speed;
        let limit = end - start;
        match self.start_playback(|output| output.start(info, &frames, start, speed, Some(limit))) {
            Ok(preview) => self.playback = Some(preview),
            Err(error) => {
                self.playback = None;
                self.activity = format!("Preview produced no sound. {error}");
            }
        }
        Ok(())
    }

    /// Carry the app-wide choices (device, scrub zoom, speed) into
    /// `settings.toml` so the next session opens the same way.
    fn store_settings(&self, update: impl FnOnce(&mut settings::Playback, &Self)) {
        let mut settings = self.session.settings.borrow_mut();
        update(&mut settings.playback, self);
        if let Err(error) = crate::settings::store(&crate::paths::app_settings(), &settings) {
            let window = self.window;
            let message = format!("Could not save playback preferences: {error}");
            App::post(move || {
                if window.is_alive() {
                    show_error(window, &message);
                }
            });
        }
    }

    /// Open the shared output stream for the selected device, replacing it
    /// when the selection changes. Opened lazily, so a window that never
    /// plays touches no audio device.
    fn ensure_output(&mut self) -> Result<()> {
        let matches = self
            .output
            .as_ref()
            .is_some_and(|output| output.matches(self.device.as_deref()));
        if !matches {
            self.output = Some(audio::Output::open(self.device.as_deref())?);
        }
        Ok(())
    }

    /// Start playback through `start` on this window's output stream. When
    /// the start fails, the stream is reopened — a stream can die when a
    /// device is unplugged or its driver restarts — and the start is retried
    /// once before the failure is reported.
    fn start_playback(
        &mut self,
        start: impl Fn(&audio::Output) -> Result<Playback>,
    ) -> Result<Playback> {
        self.ensure_output()?;
        let attempted = {
            let output = self
                .output
                .as_ref()
                .expect("ensure_output opened an output");
            start(output)
        };
        if attempted.is_ok() {
            return attempted;
        }
        self.output = None;
        self.ensure_output()?;
        let output = self
            .output
            .as_ref()
            .expect("ensure_output opened an output");
        start(output)
    }

    fn play(&mut self) -> Result<()> {
        if self.edit.frames.is_empty() {
            return Err("The timeline is empty.".into());
        }
        if self.edit.cursor == self.edit.frames.len() {
            self.edit.cursor = 0;
        }
        let info = self.format;
        let frames = self.edit.frames.clone();
        let position = self.edit.cursor;
        let speed = self.speed;
        let playback =
            self.start_playback(|output| output.start(info, &frames, position, speed, None))?;
        self.playback = Some(playback);
        Ok(())
    }

    fn set_speed(&mut self, speed: f32) {
        self.speed = speed.clamp(SPEED_MIN, SPEED_MAX);
        if let Some(playback) = &self.playback {
            playback.set_speed(self.speed);
        }
        self.update_speed_checks();
        self.store_settings(|saved, editor| saved.speed = editor.speed.clone());
        if (self.speed - 1.0).abs() < 0.001 {
            self.activity = "Playback speed 1.00x (normal pitch).".into();
        } else {
            self.activity = format!(
                "Playback speed {:.2}x. Speed changes pitch because this is vari-speed, not time stretch.",
                self.speed
            );
        }
        self.refresh();
    }

    fn change_speed(&mut self, delta: f32) {
        let speed = ((self.speed + delta) * 20.0).round() / 20.0;
        self.set_speed(speed);
    }

    fn select_device(&mut self, device: Option<String>) -> Result<()> {
        if self.device == device {
            self.update_device_checks();
            return Ok(());
        }
        let was_paused = self
            .playback
            .as_ref()
            .map(|playback| playback.sink.is_paused());
        let position = self.position();
        self.playback = None;
        self.device = device;
        self.store_settings(|saved, editor| saved.device = editor.device.clone());
        self.update_device_checks();
        self.edit.cursor = position.min(self.edit.frames.len());
        if let Some(paused) = was_paused
            && self.edit.cursor < self.edit.frames.len()
        {
            self.play()?;
            if paused && let Some(playback) = &self.playback {
                playback.sink.pause();
            }
        }
        self.activity = format!("Playback device: {}.", self.device_name());
        self.refresh();
        Ok(())
    }

    fn device_name(&self) -> &str {
        self.device.as_deref().unwrap_or("System Default")
    }

    fn update_device_checks(&self) {
        for (device, item) in &self.device_items {
            item.set_checked(device.as_deref() == self.device.as_deref());
        }
    }

    fn update_speed_checks(&self) {
        for (speed, item) in &self.speed_items {
            item.set_checked((self.speed - speed).abs() < 0.001);
        }
    }

    /// Replace `range` with `insert`, keeping the titles and the file
    /// sources exact through the splice. Every edit that moves frames funnels
    /// through here; `inserted_sources` is the pasted block's own source
    /// spans, relative to the block, so a cue point named `$file` knows
    /// which file the pasted audio came from.
    fn replace(
        &mut self,
        range: std::ops::Range<usize>,
        insert: Frames,
        inserted_sources: &[(usize, String)],
    ) {
        self.stop_here();
        self.title_anchor = None;
        if self.undo.len() == 32 {
            self.undo.remove(0);
        }
        self.undo.push(self.edit.clone());
        self.redo.clear();
        let start = range.start;
        let removed = range.len();
        let old_total = self.edit.frames.len();
        let inserted = insert.len();
        self.edit.frames.splice(range, insert);
        splice_titles(
            &mut self.edit.titles,
            start,
            start + removed,
            inserted,
            self.edit.frames.len(),
        );
        splice_spans(
            &mut self.edit.sources,
            old_total,
            start,
            start + removed,
            inserted_sources,
            inserted,
        );
        let end = start + inserted;
        self.edit.cursor = start;
        self.edit.start = (start != end).then_some(start);
        self.edit.end = (start != end).then_some(end);
        self.edit.revision = self.next_revision;
        self.next_revision += 1;
    }

    fn check_destination(&self, path: &Path) -> Result<()> {
        if same_path(path, &self.path) {
            return Err(
                "Choose a different file name. DirectCut does not overwrite the opened source."
                    .into(),
            );
        }
        // A destination CUE sheet that is the source's own sheet would replace
        // the source's markers, whether or not they were loaded into this
        // window.
        let source_cue = self.path.with_extension("cue");
        if same_path(&path.with_extension("cue"), &source_cue)
            && (self.cue.is_some() || source_cue.exists())
        {
            return Err(
                "Choose a different file name. DirectCut does not overwrite the source CUE sheet."
                    .into(),
            );
        }
        Ok(())
    }

    fn cue_snapshot(&self, path: &Path) -> Option<Vec<u8>> {
        let (_, expected) = self
            .saved_cues
            .iter()
            .find(|(saved, _)| same_path(saved, path))?;
        (fs::read(path).ok().as_ref() == Some(expected)).then(|| expected.clone())
    }

    fn remember_cue_outcome(&mut self, outcome: &ExportOutcome) {
        if let Some(removed) = &outcome.removed_cue {
            self.saved_cues
                .retain(|(saved, _)| !same_path(saved, removed));
        }
        if let Some((path, contents)) = &outcome.written_cue {
            self.saved_cues.retain(|(saved, _)| !same_path(saved, path));
            self.saved_cues.push((path.clone(), contents.clone()));
        }
    }

    fn cue_plan(
        &self,
        path: &Path,
        range: Option<std::ops::Range<usize>>,
    ) -> Option<(PathBuf, Vec<(f64, String)>)> {
        // Titles can come from a loaded CUE sheet or from cue points added
        // here; either way a timeline with titles saves with one.
        let (titles, sources, total) = match range {
            Some(range) => {
                let total = range.len();
                (
                    titles_in_range(&self.edit.titles, &range),
                    sources_in_range(&self.edit.sources, &range),
                    total,
                )
            }
            None => (
                self.edit.titles.clone(),
                self.edit.sources.clone(),
                self.edit.frames.len(),
            ),
        };
        let entries = cue_entries(&titles, &sources, self.format, total);
        (!entries.is_empty()).then(|| (path.with_extension("cue"), entries))
    }

    fn save(&mut self, choose_path: bool) -> Result<()> {
        if self.save.is_some() || self.save_selection.is_some() || self.split.is_some() {
            return Err("Audio is already being saved from this window.".into());
        }
        if self.edit.frames.is_empty() {
            return Err("The timeline is empty.".into());
        }
        let path = if choose_path || self.destination.is_none() {
            let suggested = format!(
                "{} - edited.{}",
                self.path
                    .file_stem()
                    .unwrap_or(self.path.as_os_str())
                    .to_string_lossy(),
                self.format.extension()
            );
            let Some(path) = self.modal(|| {
                FileDialog::new()
                    .title("Save edited audio")
                    .filter(&self.format.audio_label(), &[self.format.extension()])
                    .file_name(&suggested)
                    .save(Some(self.window))
            }) else {
                return Ok(());
            };
            path
        } else if let Some(destination) = self.destination.clone() {
            destination
        } else {
            return Err("Choose an output path for this save.".into());
        };
        self.check_destination(&path)?;
        let format = self.format;
        let frames = self.edit.frames.clone();
        // A CUE is expected when the source had one, or when titles were
        // added here to a file that had none.
        let cue_wanted = self.cue.is_some() || !self.edit.titles.is_empty();
        let cue = self.cue_plan(&path, None);
        // Without titles there is nothing to write into a CUE sheet. One this
        // window wrote earlier at this destination is obsolete and removed; a
        // foreign sheet is left in place and reported.
        let mut obsolete = None;
        let mut no_cue = None;
        if cue.is_none() {
            let existing = path.with_extension("cue");
            if let Some(expected) = self.cue_snapshot(&existing) {
                obsolete = Some((existing, expected));
                no_cue = Some(NoCue::Note("No title markers remain.".into()));
            } else if existing.exists() {
                no_cue = Some(NoCue::LeftInPlace(format!(
                    "The existing CUE sheet {} was left in place, so reopening this file will show its markers.",
                    name(&existing)
                )));
            } else if cue_wanted {
                no_cue = Some(NoCue::Note(
                    "No CUE sheet was written: no title markers remain.".into(),
                ));
            }
        }
        // A CUE sheet at the destination that this window did not write is
        // replaced only with the user's explicit confirmation.
        let mut expected_cue = None;
        if cue.is_some() {
            let cue_path = path.with_extension("cue");
            expected_cue = self.cue_snapshot(&cue_path);
            if expected_cue.is_none() && cue_path.exists() {
                let existing = fs::read(&cue_path)?;
                let answer = self.modal(|| {
                    message_box(
                        Some(self.window),
                        MessageKind::Warning,
                        Buttons::OkCancel,
                        "Replace CUE sheet?",
                        &format!(
                            "A CUE sheet named {} already exists. Replace it with the sheet for this save?",
                            name(&cue_path)
                        ),
                    )
                });
                if answer != Answer::Ok {
                    self.activity = "Save cancelled.".into();
                    self.refresh();
                    return Ok(());
                }
                expected_cue = Some(existing);
            }
        }
        let revision = self.edit.revision;
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("Audio export".into())
            .spawn(move || {
                let result =
                    export_with_cue(&path, format, frames, cue, no_cue, obsolete, expected_cue);
                let _ = sender.send((path, revision, result));
            })?;
        self.save = Some(receiver);
        self.activity = "Validating and saving audio...".into();
        Ok(())
    }

    fn save_selection(&mut self) -> Result<()> {
        if self.save.is_some() || self.save_selection.is_some() || self.split.is_some() {
            return Err("Audio is already being saved from this window.".into());
        }
        let range = self.edit.selection()?;
        let suggested = format!(
            "{} - selection.{}",
            self.path
                .file_stem()
                .unwrap_or(self.path.as_os_str())
                .to_string_lossy(),
            self.format.extension()
        );
        let Some(path) = self.modal(|| {
            FileDialog::new()
                .title("Save selection as audio")
                .filter(&self.format.audio_label(), &[self.format.extension()])
                .file_name(&suggested)
                .save(Some(self.window))
        }) else {
            return Ok(());
        };
        self.check_destination(&path)?;
        let format = self.format;
        let frames = self.edit.frames[range.clone()].to_vec();
        // Same rule as a full save: a CUE is expected when the source had one,
        // or when the marked range contains cue points added here.
        let cue_wanted =
            self.cue.is_some() || !titles_in_range(&self.edit.titles, &range).is_empty();
        let cue = self.cue_plan(&path, Some(range));
        let existing = path.with_extension("cue");
        let obsolete = cue
            .is_none()
            .then(|| self.cue_snapshot(&existing))
            .flatten()
            .map(|expected| (existing.clone(), expected));
        let no_cue = if cue.is_none() && obsolete.is_none() && existing.exists() {
            Some(NoCue::LeftInPlace(format!(
                "The existing CUE sheet {} was left in place, so reopening this file will show its markers.",
                name(&existing)
            )))
        } else if cue.is_none() && (cue_wanted || obsolete.is_some()) {
            Some(NoCue::Note(
                "No CUE sheet was written: the marked range contains no cue points.".into(),
            ))
        } else {
            None
        };
        // Same rule as a full save: a CUE sheet at the destination that this
        // window did not write is replaced only with explicit confirmation.
        let mut expected_cue = None;
        if cue.is_some() {
            let cue_path = path.with_extension("cue");
            expected_cue = self.cue_snapshot(&cue_path);
            if expected_cue.is_none() && cue_path.exists() {
                let existing = fs::read(&cue_path)?;
                let answer = self.modal(|| {
                    message_box(
                        Some(self.window),
                        MessageKind::Warning,
                        Buttons::OkCancel,
                        "Replace CUE sheet?",
                        &format!(
                            "A CUE sheet named {} already exists. Replace it with the sheet for this save?",
                            name(&cue_path)
                        ),
                    )
                });
                if answer != Answer::Ok {
                    self.activity = "Save cancelled.".into();
                    self.refresh();
                    return Ok(());
                }
                expected_cue = Some(existing);
            }
        }
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("Selection export".into())
            .spawn(move || {
                let result =
                    export_with_cue(&path, format, frames, cue, no_cue, obsolete, expected_cue);
                let _ = sender.send((path, result));
            })?;
        self.save_selection = Some(receiver);
        self.activity = "Validating and saving the selection...".into();
        Ok(())
    }

    fn insert_file(&mut self) -> Result<()> {
        if self.insert.is_some() {
            return Err("Audio is already being read for insertion.".into());
        }
        let Some(path) = self.modal(|| {
            FileDialog::new()
                .title("Insert audio at the playhead")
                .filter("Audio", &["mp3", "mp2", "aac"])
                .open(Some(self.window))
        }) else {
            return Ok(());
        };
        self.stop_here();
        let position = self.position();
        let revision = self.edit.revision;
        let input = path.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("Audio insert".into())
            .spawn(move || {
                let result: Result<Timeline> = fs::read(&input)
                    .map_err(Into::into)
                    .and_then(|bytes| codec::open(&bytes));
                let _ = sender.send(Insertion {
                    path: input,
                    revision,
                    position,
                    result,
                });
            })?;
        self.insert = Some(receiver);
        self.activity = format!("Reading {} for insertion...", name(&path));
        let message = self.activity.clone();
        self.speak(&message);
        Ok(())
    }

    fn duplicate(&mut self) -> Result<()> {
        let range = self.edit.selection()?;
        let copy = self.edit.frames[range.clone()].to_vec();
        let sources = sources_in_range(&self.edit.sources, &range);
        let count = copy.len();
        self.replace(range.end..range.end, copy, &sources);
        self.activity = format!("Duplicated {count} frames after the selection.");
        Ok(())
    }

    /// Export every CUE title's audio as its own file in a chosen folder.
    /// Names are the track number and the cue point's name; an existing file
    /// is never replaced: the name is claimed atomically just before the
    /// validated audio commits, and a lost claim moves to the next free
    /// numbered sibling.
    fn split_tracks(&mut self) -> Result<()> {
        if self.save.is_some() || self.save_selection.is_some() || self.split.is_some() {
            return Err("Audio is already being saved from this window.".into());
        }
        if self.edit.frames.is_empty() {
            return Err("The timeline is empty.".into());
        }
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles to split at.".into());
        }
        if let Some(first) = self.edit.titles.first().filter(|cue| cue.frame > 0) {
            if !crate::dialogs::confirm_split(
                self.window,
                &format!(
                    "The first cue point is at {}. Audio before it won't be included in the split files. Continue?",
                    time_text(self.format, first.frame)
                ),
            ) {
                return Ok(());
            }
        }
        let Some(folder) = self.modal(|| {
            FileDialog::new()
                .title("Choose a folder for the split files")
                .pick_folder(Some(self.window))
        }) else {
            return Ok(());
        };
        let ranges = split_ranges(&self.edit.titles, self.edit.frames.len());
        if ranges.is_empty() {
            return Err("No cue point lies inside the audio, so there is nothing to split.".into());
        }
        let mut files = Vec::with_capacity(ranges.len());
        for (index, range) in ranges.iter().enumerate() {
            // The track name uses the cue point's shown name, so a name
            // carrying $file is written as the file its audio came from.
            let marker = &self.edit.titles[index];
            let title = expanded_title(&marker.title, &self.edit.sources, marker.frame);
            let suggested = folder.join(format!(
                "{:02} {}.{}",
                index + 1,
                safe_name(&title, UNTITLED),
                self.format.extension()
            ));
            files.push((suggested, index + 1, title, range.clone()));
        }
        let count = files.len();
        let format = self.format;
        let source = self.path.clone();
        // Timeline frames are shared behind Arcs, so the worker clones the
        // handle list for free.
        let frames = self.edit.frames.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("Track split".into())
            .spawn(move || {
                let mut outcomes = Vec::with_capacity(files.len());
                for (suggested, number, title, range) in files {
                    let result = export_split_file(
                        suggested.clone(),
                        format,
                        frames[range.clone()].to_vec(),
                    );
                    outcomes.push(SplitOutcome {
                        suggested,
                        number,
                        title,
                        range,
                        result,
                    });
                }
                let _ = sender.send(SplitMessage {
                    source,
                    folder,
                    outcomes,
                });
            })?;
        self.split = Some(receiver);
        self.activity = format!("Splitting into {count} file(s)...");
        self.refresh();
        Ok(())
    }

    fn crop(&mut self) -> Result<()> {
        let total = self.edit.frames.len();
        let range = self.edit.selection()?;
        if range.start == 0 && range.end == total {
            self.activity = "The selection already covers the timeline.".into();
            return Ok(());
        }
        let titles = titles_in_range(&self.edit.titles, &range);
        let sources = sources_in_range(&self.edit.sources, &range);
        let frames = self.edit.frames[range.clone()].to_vec();
        self.replace(0..total, frames, &sources);
        self.edit.titles = titles;
        self.edit.start = None;
        self.edit.end = None;
        self.activity = "Cropped the timeline to the selection.".into();
        Ok(())
    }

    fn analyze(&mut self) -> Result<()> {
        if self.analyze.is_some() {
            return Err("The source is already being analyzed.".into());
        }
        if self.format.kind() != codec::Kind::Mp3 {
            return Err("Analysis reports are MP3-specific in this build.".into());
        }
        let path = self.path.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("MP3 analysis".into())
            .spawn(move || {
                let result = report::analyze_file(&path);
                let _ = sender.send(result);
            })?;
        self.analyze = Some(receiver);
        self.activity = "Analyzing the source MP3 (read-only)...".into();
        Ok(())
    }

    fn jump_to(&mut self) -> Result<()> {
        let initial = jump_initial_text(
            &self.edit.titles,
            self.format,
            self.title_anchor,
            self.position(),
        );
        let Some(text) = self.modal(|| {
            prompt(
                Some(self.window),
                "Jump to time",
                "Enter a time as ss, mm:ss, or hh:mm:ss, or a CUE title as t and its number, for example t24. The field starts at the playhead.",
                &initial,
            )
        }) else {
            return Ok(());
        };
        match parse_jump(&text)? {
            JumpTarget::Title(number) => {
                let index = self.title_index(number)?;
                let frame = self.edit.titles[index].frame;
                self.seek(frame)?;
                self.title_anchor = Some(frame);
                let message = self.title_text(index);
                self.activity = message.clone();
                self.speak(&message);
            }
            JumpTarget::Time(seconds) => {
                let target = (seconds * f64::from(self.format.rate())
                    / self.format.samples() as f64)
                    .round();
                if !target.is_finite() || target > usize::MAX as f64 {
                    return Err("That time is too large.".into());
                }
                let target = target as usize;
                let end = self.edit.frames.len();
                let message = if target >= end {
                    self.seek(end)?;
                    "Jumped to the end; the requested time is past the end.".to_string()
                } else {
                    self.seek(target)?;
                    format!("Jumped to {}.", time_text(self.format, target))
                };
                self.activity = message.clone();
                self.speak(&message);
            }
        }
        Ok(())
    }

    fn title_index(&self, number: usize) -> Result<usize> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        if number == 0 {
            return Err("Title numbers start at 1.".into());
        }
        number
            .checked_sub(1)
            .filter(|&index| index < self.edit.titles.len())
            .ok_or_else(|| {
                format!(
                    "Title {number} does not exist. This file has {} title(s).",
                    self.edit.titles.len()
                )
                .into()
            })
    }

    fn title_at(&self, frame: usize) -> Option<usize> {
        title_at_frame(&self.edit.titles, frame)
    }

    fn title_text(&self, index: usize) -> String {
        let marker = &self.edit.titles[index];
        let end = self
            .edit
            .titles
            .get(index + 1)
            .map_or(self.edit.frames.len(), |next| next.frame);
        format!(
            "Title {} of {}: {}. Starts at {}. Length {}.",
            index + 1,
            self.edit.titles.len(),
            expanded_title(&marker.title, &self.edit.sources, marker.frame),
            time_text(self.format, marker.frame),
            time_text(self.format, end.saturating_sub(marker.frame))
        )
    }

    fn next_title(&mut self) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let position = self.position();
        let total = self.edit.frames.len();
        match next_title_index(&self.edit.titles, position, total) {
            Some(index) => {
                let frame = self.edit.titles[index].frame;
                self.seek(frame)?;
                self.title_anchor = Some(frame);
                let message = self.title_text(index);
                self.activity = message.clone();
                self.speak(&message);
            }
            None => {
                // Navigation never yanks the playhead to a boundary as a
                // side effect: the file end is reached with End or
                // Ctrl+End when wanted, and playing there is its own
                // decision.
                let message = "End of timeline. No next title.";
                self.activity = message.into();
                self.speak(message);
            }
        }
        Ok(())
    }

    fn previous_title(&mut self) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let position = self.position();
        let total = self.edit.frames.len();
        match previous_title_index(&self.edit.titles, position, total) {
            Some(index) => {
                let frame = self.edit.titles[index].frame;
                self.seek(frame)?;
                self.title_anchor = Some(frame);
                let message = self.title_text(index);
                self.activity = message.clone();
                self.speak(&message);
            }
            None => {
                let message = "Start of timeline. No previous title.";
                self.activity = message.into();
                self.speak(message);
            }
        }
        Ok(())
    }

    fn find_title(&self, query: &str) -> Result<usize> {
        let query = query.trim();
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        if query.is_empty() {
            return Err("Enter a title number or part of a title name.".into());
        }
        let numbered = if query.len() > 1 && matches!(query.as_bytes()[0], b't' | b'T') {
            &query[1..]
        } else {
            query
        };
        if let Ok(number) = numbered.trim().parse::<usize>() {
            return self.title_index(number);
        }
        let needle = query.to_lowercase();
        self.edit
            .titles
            .iter()
            .position(|marker| {
                expanded_title(&marker.title, &self.edit.sources, marker.frame)
                    .to_lowercase()
                    .contains(&needle)
            })
            .ok_or_else(|| format!("No title contains \"{query}\".").into())
    }

    fn go_to_title(&mut self) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let Some(text) = self.modal(|| {
            prompt(
                Some(self.window),
                "Go to title",
                "Enter a title number, t and a number, or part of a title name.",
                "",
            )
        }) else {
            return Ok(());
        };
        let index = self.find_title(&text)?;
        let frame = self.edit.titles[index].frame;
        self.seek(frame)?;
        self.title_anchor = Some(frame);
        let message = self.title_text(index);
        self.activity = message.clone();
        self.speak(&message);
        Ok(())
    }

    fn read_titles(&self) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let total = self.edit.frames.len();
        let mut text = format!(
            "{} CUE title(s). Times start from the timeline start.\r\n\r\n",
            self.edit.titles.len()
        );
        for (index, marker) in self.edit.titles.iter().enumerate() {
            let end = self
                .edit
                .titles
                .get(index + 1)
                .map_or(total, |next| next.frame);
            text.push_str(&format!(
                "{}. {}  Start {}  Length {}\r\n",
                index + 1,
                expanded_title(&marker.title, &self.edit.sources, marker.frame),
                time_text(self.format, marker.frame),
                time_text(self.format, end.saturating_sub(marker.frame))
            ));
        }
        let title = format!("CUE titles - {}", name(&self.path));
        show_report(
            self.window,
            &title,
            "Read-only title list. The source file was not modified.",
            "Save title list",
            &report::suggested_name(&self.path, "titles"),
            text,
        );
        Ok(())
    }

    /// Snapshot the timeline for undo before a titles-only edit. Unlike a
    /// frame edit, playback can continue through it; the playhead is synced
    /// into the snapshot first, the way a frame edit's stop does, so undo
    /// restores a meaningful position. The title navigation anchor and the
    /// announce-while-playing tracker are reset, the way the Titles > Announce
    /// toggle resets them.
    fn record_title_edit(&mut self) {
        self.edit.cursor = self.position();
        self.title_anchor = None;
        self.last_title = None;
        if self.undo.len() == 32 {
            self.undo.remove(0);
        }
        self.undo.push(self.edit.clone());
        self.redo.clear();
        self.edit.revision = self.next_revision;
        self.next_revision += 1;
    }

    /// Add a cue point at the playhead. The name prompt is prefilled with the
    /// cue point that contains the playhead, so a series can be typed as
    /// "part 1", then accepted as "part 2" and "part 3". Titles can be added
    /// to any file, whether or not it arrived with a CUE sheet; a saved
    /// timeline then writes the CUE sheet beside the export.
    fn add_title(&mut self) -> Result<()> {
        let position = self.position();
        let total = self.edit.frames.len();
        if position >= total {
            return Err(
                "The playhead is at the end of the file. Cue points must lie inside the audio."
                    .into(),
            );
        }
        if self
            .edit
            .titles
            .iter()
            .any(|marker| marker.frame == position)
        {
            return Err(format!(
                "A cue point already starts at {}.",
                time_text(self.format, position)
            )
            .into());
        }
        let previous = self
            .title_at(position)
            .map_or(String::new(), |index| self.edit.titles[index].title.clone());
        let Some(name) = self.modal(|| {
            prompt(
                Some(self.window),
                "Add cue point",
                &format!(
                    "Name for the cue point at {}. Leave it blank for {UNTITLED}. The name may carry $file, which stands for the file the audio at it came from.",
                    time_text(self.format, position)
                ),
                &previous,
            )
        }) else {
            return Ok(());
        };
        self.record_title_edit();
        let index = insert_title(&mut self.edit.titles, position, &name);
        let marker = &self.edit.titles[index];
        let message = format!(
            "Added cue point {} ({}) at {}.",
            index + 1,
            expanded_title(&marker.title, &self.edit.sources, marker.frame),
            time_text(self.format, position)
        );
        self.activity = message.clone();
        self.speak(&message);
        Ok(())
    }

    /// Rename the cue point containing the playhead: the one the navigation
    /// and delete commands address.
    fn rename_title(&mut self) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let position = self.position();
        let Some(index) = self.title_at(position) else {
            return Err(
                "The playhead is before the first cue point, so there is nothing to rename here."
                    .into(),
            );
        };
        let old = self.edit.titles[index].title.clone();
        let old_display = expanded_title(&old, &self.edit.sources, self.edit.titles[index].frame);
        let Some(name) = self.modal(|| {
            prompt(
                Some(self.window),
                "Rename cue point",
                &format!(
                    "Name for cue point {} at {}. The name may carry $file, which stands for the file the audio at it came from.",
                    index + 1,
                    time_text(self.format, self.edit.titles[index].frame)
                ),
                &old,
            )
        }) else {
            return Ok(());
        };
        let title = title_name(&name);
        if title == old {
            let message = format!("Cue point {} is already named {old_display}.", index + 1);
            self.activity = message.clone();
            self.speak(&message);
            return Ok(());
        }
        self.record_title_edit();
        self.edit.titles[index].title = title.into();
        let new_display = expanded_title(
            &self.edit.titles[index].title,
            &self.edit.sources,
            self.edit.titles[index].frame,
        );
        let message = format!(
            "Renamed cue point {} from {old_display} to {new_display}.",
            index + 1
        );
        self.activity = message.clone();
        self.speak(&message);
        Ok(())
    }

    /// Delete the cue point containing the playhead: the last one that starts
    /// at or before it, exactly the title the navigation commands address.
    fn delete_title(&mut self) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let position = self.position();
        let Some(index) = self.title_at(position) else {
            return Err(
                "The playhead is before the first cue point, so there is nothing to delete here."
                    .into(),
            );
        };
        self.record_title_edit();
        let marker = self.edit.titles.remove(index);
        let message = format!(
            "Deleted cue point {} ({} at {}). {} cue point(s) remain.",
            index + 1,
            expanded_title(&marker.title, &self.edit.sources, marker.frame),
            time_text(self.format, marker.frame),
            self.edit.titles.len()
        );
        self.activity = message.clone();
        self.speak(&message);
        Ok(())
    }

    /// Move the cue point nearest the playhead onto the playhead.
    fn move_nearest_title(&mut self) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let position = self.position();
        let total = self.edit.frames.len();
        if position >= total {
            return Err(
                "The playhead is at the end of the file. Cue points must lie inside the audio."
                    .into(),
            );
        }
        let Some(index) = nearest_title_index(&self.edit.titles, position) else {
            return Err("This file has no CUE titles.".into());
        };
        let from = self.edit.titles[index].frame;
        let title = expanded_title(&self.edit.titles[index].title, &self.edit.sources, position);
        if from == position {
            let message = format!(
                "Cue point {} is already at {}.",
                index + 1,
                time_text(self.format, position)
            );
            self.activity = message.clone();
            self.speak(&message);
            return Ok(());
        }
        self.record_title_edit();
        self.edit.titles[index].frame = position;
        // Moving one marker cannot land on another: a marker already at the
        // playhead would have been the nearest one. The moved marker may now
        // precede markers it used to follow, so restore the sorted order.
        self.edit.titles.sort_by_key(|marker| marker.frame);
        let message = format!(
            "Moved cue point ({title}) from {} to {}.",
            time_text(self.format, from),
            time_text(self.format, position)
        );
        self.activity = message.clone();
        self.speak(&message);
        Ok(())
    }

    /// Shift every cue point by one second, for aligning titles that a station
    /// reports a fixed amount of time before or after the audio changes.
    fn shift_titles(&mut self, seconds: i32) -> Result<()> {
        if self.edit.titles.is_empty() {
            return Err("This file has no CUE titles.".into());
        }
        let total = self.edit.frames.len();
        let delta = seconds as isize * frames_per_second(self.format) as isize;
        let before = self.edit.titles.len();
        let shifted = shifted_titles(&self.edit.titles, delta, total);
        if shifted.len() < before
            && self.modal(|| {
                message_box(
                    Some(self.window),
                    MessageKind::Warning,
                    Buttons::OkCancel,
                    "Merge cue points?",
                    "Shifting would merge cue points at the file boundary. Continue?",
                )
            }) != Answer::Ok
        {
            return Ok(());
        }
        self.record_title_edit();
        self.edit.titles = shifted;
        let after = self.edit.titles.len();
        let direction = if seconds > 0 { "forward" } else { "back" };
        let mut message = format!(
            "All cue points shifted {direction} one second. {} cue point(s) now.",
            after
        );
        if after < before {
            message.push_str(" Cue points that met keep the later one.");
        }
        self.activity = message.clone();
        self.speak(&message);
        Ok(())
    }

    /// Resolve the frame for a marker command. Right after a title jump the
    /// playhead has already moved past the title start, so use the navigation
    /// anchor; otherwise use the live playhead position.
    fn marker_position(&mut self) -> usize {
        let position = self.position();
        let anchor = self.title_anchor.take();
        marker_frame(&self.edit.titles, self.format, anchor, position)
    }

    fn mark_start(&mut self) {
        let frame = self.marker_position();
        let cleared = self.edit.end.is_some_and(|end| end <= frame);
        if cleared {
            self.edit.end = None;
        }
        self.edit.start = Some(frame);
        let mut message = format!("Start marker set at {}.", time_text(self.format, frame));
        match self.edit.end {
            Some(end) if end > frame => message.push_str(&format!(
                " Selection length {}.",
                time_text(self.format, end - frame)
            )),
            _ if cleared => message.push_str(" The end marker was cleared."),
            _ => message.push_str(" Set an end marker after it."),
        }
        self.activity = message;
        self.speak(&self.activity);
    }

    fn mark_end(&mut self) {
        let frame = self.marker_position();
        let cleared = self.edit.start.is_some_and(|start| start >= frame);
        if cleared {
            self.edit.start = None;
        }
        self.edit.end = Some(frame);
        let mut message = format!("End marker set at {}.", time_text(self.format, frame));
        match self.edit.start {
            Some(start) if start < frame => message.push_str(&format!(
                " Selection length {}.",
                time_text(self.format, frame - start)
            )),
            _ if cleared => {
                message.push_str(" The start marker was cleared. Set a start marker before it.")
            }
            _ => message.push_str(" Set a start marker before it."),
        }
        self.activity = message;
        self.speak(&self.activity);
    }

    /// Move one selection edge by the current scrub step, the way Reaper's
    /// bracket keys trim a time selection. The playhead follows the moved edge
    /// and scrubs there, so the trim is audible by ear. A start edge pushed to
    /// or past the end marker clears it, and the other way around, the same
    /// rule the marker keys follow, so a selection is always valid.
    fn nudge_marker(&mut self, start: bool, delta: i32) -> Result<()> {
        let Some(frame) = (if start {
            self.edit.start
        } else {
            self.edit.end
        }) else {
            let (name, key) = if start { ("start", '[') } else { ("end", ']') };
            self.activity = format!("The {name} marker is unset. Set it with {key}.");
            self.speak(&self.activity);
            return Ok(());
        };
        let total = self.edit.frames.len();
        let moved = frame
            .saturating_add_signed(delta as isize * self.scrub_step as isize)
            .min(total);
        if moved == frame {
            let message = format!(
                "The {} marker is already at the {} of the timeline.",
                if start { "start" } else { "end" },
                if delta < 0 { "beginning" } else { "end" }
            );
            self.activity = message;
            self.speak(&self.activity);
            return Ok(());
        }
        let cleared = if start {
            let cleared = self.edit.end.is_some_and(|end| end <= moved);
            if cleared {
                self.edit.end = None;
            }
            self.edit.start = Some(moved);
            cleared
        } else {
            let cleared = self.edit.start.is_some_and(|start| start >= moved);
            if cleared {
                self.edit.start = None;
            }
            self.edit.end = Some(moved);
            cleared
        };
        self.scrub(moved)?;
        let mut message = format!(
            "{} marker moved to {}.",
            if start { "Start" } else { "End" },
            time_text(self.format, moved)
        );
        if start {
            match self.edit.end {
                Some(end) if end > moved => message.push_str(&format!(
                    " Selection length {}.",
                    time_text(self.format, end - moved)
                )),
                _ if cleared => message.push_str(" The end marker was cleared."),
                _ => message.push_str(" Set an end marker after it."),
            }
        } else {
            match self.edit.start {
                Some(start) if start < moved => message.push_str(&format!(
                    " Selection length {}.",
                    time_text(self.format, moved - start)
                )),
                _ if cleared => {
                    message.push_str(" The start marker was cleared. Set a start marker before it.")
                }
                _ => message.push_str(" Set a start marker before it."),
            }
        }
        self.activity = message;
        self.speak(&self.activity);
        Ok(())
    }

    /// Speak the playhead position and current title on demand, the way
    /// OSARA's cursor report works, without a modal dialog.
    fn report_position(&mut self) {
        let mut message = format!(
            "Position {} of {}.",
            time_text(self.format, self.position()),
            time_text(self.format, self.edit.frames.len())
        );
        if !self.edit.titles.is_empty() {
            match self.title_at(self.position()) {
                Some(index) => {
                    let marker = &self.edit.titles[index];
                    message.push_str(&format!(
                        " Title {} of {}: {}.",
                        index + 1,
                        self.edit.titles.len(),
                        expanded_title(&marker.title, &self.edit.sources, marker.frame)
                    ))
                }
                None => {
                    message.push_str(&format!(" Before title 1 of {}.", self.edit.titles.len()))
                }
            }
        }
        self.activity = message;
        self.speak(&self.activity);
    }

    /// Speak the time selection on demand: its start, end, and length, or
    /// which marker is still missing.
    fn report_selection(&mut self) {
        let message = match (self.edit.start, self.edit.end) {
            (Some(start), Some(end)) if start < end => format!(
                "Selection from {} to {}, length {}.",
                time_text(self.format, start),
                time_text(self.format, end),
                time_text(self.format, end - start)
            ),
            (Some(start), _) => format!(
                "Start marker {}. Set an end marker after it.",
                time_text(self.format, start)
            ),
            (_, Some(end)) => format!(
                "End marker {}. Set a start marker before it.",
                time_text(self.format, end)
            ),
            _ => "No time selection.".into(),
        };
        self.activity = message;
        self.speak(&self.activity);
    }

    /// Home and End without a complete selection: the request is answered
    /// aloud, not silently turned into a file-boundary jump.
    fn selection_missing(&mut self, what: &str) {
        let message = format!("No time selection to jump to: {what}");
        self.activity = message.clone();
        self.speak(&message);
    }

    fn run(&mut self, command: Command) -> Result<()> {
        use Command::*;
        match command {
            Open | Close => unreachable!(),
            Save | SaveAs => self.save(matches!(command, SaveAs))?,
            SaveSelection => self.save_selection()?,
            SplitTracks => self.split_tracks()?,
            Analyze => self.analyze()?,
            InsertFile => self.insert_file()?,
            Duplicate => self.duplicate()?,
            Crop => self.crop()?,
            Beginning => self.seek(0)?,
            End => self.seek(self.edit.frames.len())?,
            Move(delta, seconds) => {
                let step = if seconds {
                    frames_per_second(self.format)
                } else {
                    self.scrub_step
                };
                // Bounded playback (a snippet or a loop) plays from the
                // cursor: nudge from the cursor itself, not the live playback
                // position, so repeated moves walk the audio one step per
                // press instead of drifting with what is sounding.
                let base = match &self.playback {
                    Some(p) if p.snippet => self.edit.cursor,
                    _ => self.position(),
                };
                let target = base.saturating_add_signed(delta as isize * step as isize);
                self.scrub(target)?;
            }
            StartMarker => self.mark_start(),
            EndMarker => self.mark_end(),
            GoStart | GoEnd => {
                let marker = if matches!(command, GoStart) {
                    self.edit.start
                } else {
                    self.edit.end
                };
                self.seek(marker.ok_or("That marker is unset.")?)?;
            }
            SelectionHome | SelectionEnd => {
                // Home and End address the selection. Without a complete one
                // they say so instead of silently turning into a file
                // boundary jump: the file boundaries are Ctrl+Home and
                // Ctrl+End, so an overshot scrub or a stray press cannot
                // masquerade as a selection jump.
                let to_start = matches!(command, SelectionHome);
                match (self.edit.start, self.edit.end) {
                    (Some(start), Some(end)) if start < end => {
                        self.seek(if to_start { start } else { end })?;
                    }
                    (Some(_), None) => {
                        self.selection_missing(
                            "the end marker is missing; press ] after the start to complete the selection",
                        );
                    }
                    (None, Some(_)) => {
                        self.selection_missing(
                            "the start marker is missing; press [ before the end to complete the selection",
                        );
                    }
                    (_, _) => {
                        self.selection_missing(
                            "mark a range with [ and ]; Ctrl+Home and Ctrl+End go to the start and end of the file",
                        );
                    }
                }
            }
            SelectionStartBack => self.nudge_marker(true, -1)?,
            SelectionStartForward => self.nudge_marker(true, 1)?,
            SelectionEndBack => self.nudge_marker(false, -1)?,
            SelectionEndForward => self.nudge_marker(false, 1)?,
            Clear => {
                // Escape is easy to press with nothing marked, so the
                // answer says what actually happened: the markers were
                // removed, or there was nothing to remove. A no-op press
                // also leaves the title-jump anchor alone, so a marker set
                // right after a title jump still snaps to that title.
                let had_selection = self.edit.start.is_some() || self.edit.end.is_some();
                if had_selection {
                    self.edit.start = None;
                    self.edit.end = None;
                    self.title_anchor = None;
                    self.freeze_status = false;
                    self.activity = "Time selection cleared.".into();
                } else {
                    self.activity = "No time selection to clear.".into();
                }
                let message = self.activity.clone();
                self.speak(&message);
            }
            SelectAll => {
                self.edit.start = Some(0);
                self.edit.end = Some(self.edit.frames.len());
            }
            Cut | Copy | Delete => {
                let range = self.edit.selection()?;
                let length = time_text(self.format, range.len());
                if !matches!(command, Delete) {
                    let sources = sources_in_range(&self.edit.sources, &range);
                    *self.session.clipboard.borrow_mut() = Some(Clipboard {
                        info: self.format,
                        frames: self.edit.frames[range.clone()].to_vec(),
                        sources,
                    });
                    self.activity =
                        format!("Copied selection of {length} to the DirectCut clipboard.");
                    if matches!(command, Copy) {
                        let message = self.activity.clone();
                        self.speak(&message);
                    }
                }
                if !matches!(command, Copy) {
                    self.replace(range, Vec::new(), &[]);
                    self.activity = if matches!(command, Cut) {
                        format!("Cut selection of {length}. Later audio moved earlier.")
                    } else {
                        format!("Deleted selection of {length}. Later audio moved earlier.")
                    };
                    let message = self.activity.clone();
                    self.speak(&message);
                }
            }
            Paste => {
                let clip = self
                    .session
                    .clipboard
                    .borrow()
                    .clone()
                    .ok_or("The DirectCut clipboard is empty.")?;
                check_splice_format(self.format, clip.info)?;
                let position = self.position();
                let range = self.edit.selection().unwrap_or(position..position);
                let replaced = range.start < range.end;
                let at = range.start;
                if replaced && self.replace_declined(&range, clip.frames.len()) {
                    self.activity = "The paste was cancelled.".into();
                    let message = self.activity.clone();
                    self.speak(&message);
                    return Ok(());
                }
                if self.mixing_bitrates(&clip.frames) {
                    self.activity = "The paste was cancelled.".into();
                    let message = self.activity.clone();
                    self.speak(&message);
                    return Ok(());
                }
                let count = clip.frames.len();
                let replaced_length = range.len();
                let label = clip
                    .sources
                    .first()
                    .map(|(_, label)| label.clone())
                    .unwrap_or_default();
                let same_before =
                    source_at(&self.edit.sources, at).is_some_and(|source| source == label);
                self.replace(range, clip.frames, &clip.sources);
                let lead = if replaced {
                    format!(
                        "Pasted {} frames ({}) over {} of audio at {}. The timeline is now {} long.",
                        count,
                        time_text(self.format, count),
                        time_text(self.format, replaced_length),
                        time_text(self.format, at),
                        time_text(self.format, self.edit.frames.len()),
                    )
                } else {
                    format!(
                        "Pasted {} frames ({}) at {}. The timeline is now {} long.",
                        count,
                        time_text(self.format, count),
                        time_text(self.format, at),
                        time_text(self.format, self.edit.frames.len()),
                    )
                };
                self.activity = self.after_combine(at, same_before, lead);
                let message = self.activity.clone();
                self.speak(&message);
            }
            Undo => {
                if let Some(edit) = self.undo.pop() {
                    self.stop_here();
                    self.title_anchor = None;
                    self.redo.push(std::mem::replace(&mut self.edit, edit));
                }
            }
            Redo => {
                if let Some(edit) = self.redo.pop() {
                    self.stop_here();
                    self.title_anchor = None;
                    self.undo.push(std::mem::replace(&mut self.edit, edit));
                }
            }
            PlayStop => {
                match self.playback.as_ref().map(|p| p.sink.is_paused()) {
                    // Space resumes a pause from the paused position, the way
                    // Reaper's space bar does.
                    Some(true) => {
                        if let Some(p) = &self.playback {
                            p.sink.play();
                        }
                    }
                    Some(false) => {
                        if let Some(p) = self.playback.take() {
                            self.edit.cursor = p.stop_return;
                        }
                    }
                    None => self.play()?,
                }
            }
            PausePlay => {
                match self.playback.as_ref().map(|p| p.sink.is_paused()) {
                    Some(true) => {
                        if let Some(p) = &self.playback {
                            p.sink.play();
                        }
                    }
                    Some(false) => {
                        // Pausing snaps the playhead to the paused position,
                        // like Reaper's pause key drops the edit cursor.
                        let position = self.position();
                        if let Some(p) = &mut self.playback {
                            p.sink.pause();
                            p.stop_return = position;
                        }
                        self.edit.cursor = position;
                    }
                    None => self.play()?,
                }
            }
            Preview => self.preview_selection()?,
            SpeedUp => self.change_speed(0.25),
            SpeedDown => self.change_speed(-0.25),
            ResetSpeed => self.set_speed(1.0),
            ZoomIn | ZoomOut => {
                let zoom_in = matches!(command, ZoomIn);
                let before = self.scrub_step;
                let zoomed = zoomed_scrub_step(before, zoom_in);
                self.scrub_step = if zoom_in {
                    zoomed
                } else {
                    zoomed.min(frames_per_second(self.format))
                };
                self.activity = if self.scrub_step == before {
                    if zoom_in {
                        "Scrub step is already 1 frame.".into()
                    } else {
                        "Scrub step is already 1 second.".into()
                    }
                } else {
                    format!("Scrub step {}.", scrub_text(self.scrub_step, self.format))
                };
                self.store_settings(|saved, editor| saved.scrub_step = editor.scrub_step.clone());
                let message = self.activity.clone();
                self.speak(&message);
            }
            JumpTo => self.jump_to()?,
            ReportPosition => self.report_position(),
            ReportSelection => self.report_selection(),
            PreviousTitle => self.previous_title()?,
            NextTitle => self.next_title()?,
            GoToTitle => self.go_to_title()?,
            ReadTitles => self.read_titles()?,
            AddTitle => self.add_title()?,
            RenameTitle => self.rename_title()?,
            DeleteTitle => self.delete_title()?,
            MoveNearestTitle => self.move_nearest_title()?,
            ShiftTitlesBack => self.shift_titles(-1)?,
            ShiftTitlesForward => self.shift_titles(1)?,
            Status => {
                let text = self.status_text();
                self.modal(|| {
                    message_box(
                        Some(self.window),
                        MessageKind::Info,
                        Buttons::Ok,
                        "Timeline status",
                        &text,
                    );
                });
            }
            Help => {
                show_report(
                    self.window,
                    "Keyboard Shortcuts - DirectCut RS",
                    "Read or save the keyboard shortcuts.",
                    "Save keyboard shortcuts",
                    "DirectCut RS hotkeys.txt",
                    hotkeys_document(),
                );
            }
        }
        self.refresh();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Info, JumpTarget, NoCue, REPLACE_ASK_SECONDS, TitleMarker, bitrates_differ,
        boundary_ask_wanted, cue_entries, cue_titles, expanded_title, export_split_file,
        export_with_cue, frames_per_second, insert_title, is_supported_file, jump_initial_text,
        marker_frame, nearest_title_index, next_title_index, parse_jump, parse_timestamp,
        previous_title_index, replace_needs_confirm, scrub_snap_frames, scrub_text, shifted_titles,
        source_at, source_name, sources_in_range, speed_steps, splice_spans, splice_titles,
        split_ranges, title_at_frame, titles_in_range, unique_split_path, zoomed_scrub_step,
    };
    use crate::codec::{self, Frame, Frames};
    use std::{path::Path, sync::Arc};

    fn marker(frame: usize, title: &str) -> TitleMarker {
        TitleMarker {
            frame,
            title: title.into(),
        }
    }

    fn source(frame: usize, name: &str) -> (usize, String) {
        (frame, name.to_string())
    }

    #[test]
    fn combining_audio_rejects_different_sample_rates() {
        let destination = Info::test(44_100, 2);
        let incoming = Info::test(48_000, 2);
        let error = super::check_splice_format(destination, incoming)
            .unwrap_err()
            .to_string();
        assert!(error.contains("48000 Hz"));
        assert!(error.contains("44100 Hz"));
        assert!(super::check_splice_format(destination, destination).is_ok());
    }

    /// The split naming rule: a free suggestion is used as is; a taken one
    /// becomes the first free numbered sibling.
    #[test]
    fn unique_split_path_skips_taken_names() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("01 song.mp3");
        assert_eq!(unique_split_path(base.clone()), base);
        std::fs::write(&base, b"x").unwrap();
        assert_eq!(
            unique_split_path(base.clone()),
            directory.path().join("01 song 2.mp3")
        );
        std::fs::write(directory.path().join("01 song 2.mp3"), b"x").unwrap();
        assert_eq!(
            unique_split_path(base.clone()),
            directory.path().join("01 song 3.mp3")
        );
    }

    /// A real export failure is reported as an error rather than a rename,
    /// and nothing is written to the suggested name.
    #[test]
    fn export_split_file_reports_real_failures() {
        let directory = tempfile::tempdir().unwrap();
        let suggested = directory.path().join("01 x.mp3");
        let frames = vec![Arc::new(Frame::Data(vec![0u8; 10].into_boxed_slice()))];
        let error =
            export_split_file(suggested.clone(), Info::test(44_100, 2), frames).unwrap_err();
        assert!(!error.is_empty());
        assert!(!suggested.exists());
    }

    /// A CUE write failure still saves the audio and is reported as a
    /// warning inside a successful outcome; the editor decides cleanliness.
    #[test]
    fn export_with_cue_reports_cue_failures_without_blocking_audio() {
        let directory = tempfile::tempdir().unwrap();
        let audio = directory.path().join("show.aac");
        let frames = vec![Arc::new(Frame::Data(vec![1, 2].into_boxed_slice()))];
        let info = Info::Aac {
            profile: 1,
            rate: 44_100,
            channels: 2,
            decodable: false,
        };
        let cue = Some((
            directory.path().join("missing").join("show.cue"),
            vec![(0.0, "A".to_string())],
        ));
        let outcome = export_with_cue(&audio, info, frames.clone(), cue, None, None, None).unwrap();
        assert!(outcome.cue.is_none());
        assert!(outcome.cue_error.as_deref().unwrap().contains("show.cue"));
        assert_eq!(std::fs::read(&audio).unwrap(), vec![1, 2]);
    }

    /// When no titles remain, a CUE sheet this window wrote earlier is
    /// removed with the save; a foreign sheet is left in place and reported.
    #[test]
    fn export_with_cue_removes_an_obsolete_cue_sheet() {
        let directory = tempfile::tempdir().unwrap();
        let audio = directory.path().join("show.aac");
        let stale = directory.path().join("show.cue");
        std::fs::write(&stale, b"old markers").unwrap();
        let frames = vec![Arc::new(Frame::Data(vec![3].into_boxed_slice()))];
        let info = Info::Aac {
            profile: 1,
            rate: 44_100,
            channels: 2,
            decodable: false,
        };
        let outcome = export_with_cue(
            &audio,
            info,
            frames.clone(),
            None,
            Some(NoCue::Note(
                "No CUE sheet was written: no title markers remain.".into(),
            )),
            Some((stale.clone(), b"old markers".to_vec())),
            None,
        )
        .unwrap();
        assert_eq!(outcome.removed_cue.as_deref(), Some(stale.as_path()));
        assert!(!stale.exists());
        assert!(outcome.cue_error.is_none());
        std::fs::write(&stale, b"foreign").unwrap();
        let outcome = export_with_cue(
            &audio,
            info,
            frames,
            None,
            Some(NoCue::LeftInPlace(
                "The existing CUE sheet was left in place, so reopening this file will show its markers.".into(),
            )),
            None,
            None,
        )
        .unwrap();
        assert!(outcome.removed_cue.is_none());
        assert_eq!(std::fs::read(&stale).unwrap(), b"foreign");
        assert!(
            outcome
                .cue_error
                .as_deref()
                .unwrap()
                .contains("left in place")
        );
    }

    #[test]
    fn split_ranges_cut_at_titles_and_skip_leading_audio() {
        let titles = vec![marker(0, "A"), marker(31, "B"), marker(59, "C")];
        assert_eq!(split_ranges(&titles, 148), vec![0..31, 31..59, 59..148]);

        // A CUE whose first entry is not at the start: leading audio belongs
        // to no track and is left out, the way Trim Archive leaves it out.
        let titles = vec![marker(10, "A"), marker(50, "B")];
        assert_eq!(split_ranges(&titles, 80), vec![10..50, 50..80]);

        // One title covers everything after it.
        let titles = vec![marker(0, "A")];
        assert_eq!(split_ranges(&titles, 100), vec![0..100]);
        let titles = vec![marker(30, "A")];
        assert_eq!(split_ranges(&titles, 100), vec![30..100]);

        // A title at the very end holds no audio and gets no file.
        let titles = vec![marker(50, "A"), marker(100, "B")];
        assert_eq!(split_ranges(&titles, 100), vec![50..100]);
    }

    #[test]
    fn supported_files_match_the_open_filter() {
        assert!(is_supported_file(Path::new("a.mp3")));
        assert!(is_supported_file(Path::new("A.MP3")));
        assert!(is_supported_file(Path::new("b.mp2")));
        assert!(is_supported_file(Path::new("c.aac")));
        assert!(!is_supported_file(Path::new("d.mp4")));
        assert!(!is_supported_file(Path::new("e.txt")));
        assert!(!is_supported_file(Path::new("mp3")));
    }

    #[test]
    fn parses_seconds_minutes_and_hours() {
        assert_eq!(parse_timestamp("5").unwrap(), 5.0);
        assert_eq!(parse_timestamp("5.5").unwrap(), 5.5);
        assert_eq!(parse_timestamp("2:03").unwrap(), 123.0);
        assert_eq!(parse_timestamp("1:02:03").unwrap(), 3723.0);
        assert_eq!(parse_timestamp("90").unwrap(), 90.0);
        assert_eq!(parse_timestamp("90:00").unwrap(), 5400.0);
        assert_eq!(parse_timestamp(" 1 : 02 : 03.5 ").unwrap(), 3723.5);
    }

    #[test]
    fn rejects_malformed_times() {
        for text in ["", ":", "1:", ":2", "1:2:3:4", "one", "-1", "1:-2"] {
            assert!(
                parse_timestamp(text).is_err(),
                "{text:?} should be rejected"
            );
        }
        assert!(parse_timestamp("1:60").is_err());
        assert!(parse_timestamp("1:60:00").is_err());
        assert!(parse_timestamp("1:00:60").is_err());
    }

    #[test]
    fn parses_title_jumps() {
        assert_eq!(parse_jump("t24").unwrap(), JumpTarget::Title(24));
        assert_eq!(parse_jump(" T1 ").unwrap(), JumpTarget::Title(1));
        assert_eq!(parse_jump("24").unwrap(), JumpTarget::Time(24.0));
        assert_eq!(parse_jump("1:02:03").unwrap(), JumpTarget::Time(3723.0));
        for text in ["t0", "t", "t2:03", "t-1"] {
            assert!(parse_jump(text).is_err(), "{text:?} should be rejected");
        }
    }

    #[test]
    fn cue_titles_snap_to_frames_and_keep_the_later_duplicate() {
        let format = Info::test(44_100, 2);
        let frame = format.samples() as f64 / f64::from(format.rate());
        let entries = vec![
            (0.0, "A".into()),
            (frame * 5.0 + 0.000_001, "B".into()),
            (frame * 5.0, "C".into()),
            (frame * 10.0, "Past end".into()),
        ];
        assert_eq!(
            cue_titles(entries, format, 10),
            vec![marker(0, "A"), marker(5, "C")]
        );
    }

    #[test]
    fn splice_titles_shifts_later_titles_and_drops_enclosed_ones() {
        let mut titles = vec![
            marker(0, "A"),
            marker(5, "B"),
            marker(10, "C"),
            marker(15, "D"),
        ];
        splice_titles(&mut titles, 5, 15, 0, 10);
        assert_eq!(titles, vec![marker(0, "A"), marker(5, "D")]);

        let mut titles = vec![marker(0, "A"), marker(10, "B")];
        splice_titles(&mut titles, 5, 5, 3, 23);
        assert_eq!(titles, vec![marker(0, "A"), marker(13, "B")]);

        let mut titles = vec![marker(0, "A"), marker(10, "B")];
        splice_titles(&mut titles, 10, 10, 3, 23);
        assert_eq!(titles, vec![marker(0, "A"), marker(10, "B")]);

        let mut titles = vec![marker(0, "A"), marker(10, "B"), marker(20, "C")];
        splice_titles(&mut titles, 8, 12, 0, 30);
        assert_eq!(titles, vec![marker(0, "A"), marker(16, "C")]);
    }

    #[test]
    fn deleting_audio_ripples_later_cue_points_by_the_deleted_length() {
        // A stutter is cut out between cue points, the way a careful edit
        // removes a stumble from speech: the cue points before the cut keep
        // their frames, and every cue point after it moves back by exactly
        // the deleted length — in frames, and therefore in time.
        let mut titles = vec![
            marker(0, "A"),
            marker(100, "B"),
            marker(250, "C"),
            marker(400, "D"),
        ];
        // What Delete Selection runs on a 450-frame timeline:
        // replace(100..150, []), leaving 400 frames.
        splice_titles(&mut titles, 100, 150, 0, 400);
        assert_eq!(
            titles,
            vec![
                marker(0, "A"),
                marker(100, "B"),
                marker(200, "C"),
                marker(350, "D"),
            ]
        );
    }

    #[test]
    fn a_cue_point_at_the_deleted_ranges_end_lands_on_the_splice() {
        // Deleting a whole track: its own cue point vanishes, the following
        // track's cue point lands exactly on the splice, and the track before
        // the deleted one keeps its position.
        let mut titles = vec![
            marker(0, "A"),
            marker(10, "B"),
            marker(20, "C"),
            marker(30, "D"),
        ];
        splice_titles(&mut titles, 10, 20, 0, 40);
        assert_eq!(
            titles,
            vec![marker(0, "A"), marker(10, "C"), marker(20, "D")]
        );
    }

    #[test]
    fn rippled_cue_points_write_exact_cue_times() {
        // Ripple moves markers by whole frames and CUE seconds derive from
        // frames, so a delete shifts every later track start by exactly the
        // deleted duration, never a rounded approximation.
        let format = Info::test(44_100, 2);
        let mut titles = vec![marker(0, "A"), marker(400, "B")];
        // Delete frames 100..200, exactly one second of audio.
        splice_titles(&mut titles, 100, 200, 0, 900);
        assert_eq!(titles, vec![marker(0, "A"), marker(300, "B")]);
        let entries = cue_entries(&titles, &[], format, 900);
        // The same multiply-then-divide the CUE writer uses.
        let second = 300.0 * format.samples() as f64 / f64::from(format.rate());
        assert_eq!(
            entries,
            vec![(0.0, "A".to_string()), (second, "B".to_string())]
        );
    }

    #[test]
    fn cue_points_insert_in_sorted_position() {
        let mut titles = vec![marker(100, "A"), marker(500, "B")];
        assert_eq!(insert_title(&mut titles, 300, " part 2 "), 1);
        assert_eq!(
            titles,
            vec![marker(100, "A"), marker(300, "part 2"), marker(500, "B")]
        );
        assert_eq!(insert_title(&mut titles, 0, ""), 0);
        assert_eq!(insert_title(&mut titles, 1000, "part 3"), 4);
        assert_eq!(
            titles,
            vec![
                marker(0, "Untitled"),
                marker(100, "A"),
                marker(300, "part 2"),
                marker(500, "B"),
                marker(1000, "part 3"),
            ]
        );
    }

    #[test]
    fn nearest_cue_point_prefers_the_earlier_tie() {
        let titles = vec![marker(100, "A"), marker(500, "B"), marker(900, "C")];
        assert_eq!(nearest_title_index(&titles, 100), Some(0));
        assert_eq!(nearest_title_index(&titles, 200), Some(0));
        assert_eq!(nearest_title_index(&titles, 300), Some(0));
        assert_eq!(nearest_title_index(&titles, 301), Some(1));
        assert_eq!(nearest_title_index(&titles, 800), Some(2));
        assert_eq!(nearest_title_index(&titles, 1000), Some(2));
        assert_eq!(nearest_title_index(&[], 5), None);
    }

    #[test]
    fn shifting_cue_points_clamps_into_the_timeline_and_keeps_the_later_one() {
        let titles = vec![marker(100, "A"), marker(200, "B")];
        assert_eq!(
            shifted_titles(&titles, 38, 1000),
            vec![marker(138, "A"), marker(238, "B")]
        );
        // Negative shifts clamp at the start; both markers meet at frame 0 and
        // the later one survives.
        let titles = vec![marker(0, "A"), marker(5, "B")];
        assert_eq!(shifted_titles(&titles, -10, 1000), vec![marker(0, "B")]);
        // Positive shifts clamp at the last frame of the timeline.
        let titles = vec![marker(0, "A"), marker(5, "B")];
        assert_eq!(shifted_titles(&titles, 10, 8), vec![marker(7, "B")]);
    }

    #[test]
    fn crop_titles_keep_intersecting_titles_and_synthesize_the_holder() {
        let titles = vec![marker(0, "A"), marker(10, "B"), marker(20, "C")];
        assert_eq!(
            titles_in_range(&titles, &(5..25)),
            vec![marker(0, "A"), marker(5, "B"), marker(15, "C")]
        );
        assert_eq!(
            titles_in_range(&titles, &(10..25)),
            vec![marker(0, "B"), marker(10, "C")]
        );
        assert_eq!(titles_in_range(&titles, &(10..12)), vec![marker(0, "B")]);
    }

    #[test]
    fn title_navigation_is_relative_to_the_containing_title() {
        let titles = vec![marker(0, "A"), marker(10, "B"), marker(20, "C")];
        // Next skips the containing title, including from its exact start.
        assert_eq!(next_title_index(&titles, 0, 30), Some(1));
        assert_eq!(next_title_index(&titles, 5, 30), Some(1));
        assert_eq!(next_title_index(&titles, 10, 30), Some(2));
        assert_eq!(next_title_index(&titles, 25, 30), None);
        assert_eq!(next_title_index(&titles, 30, 30), None);
        // Previous goes to the title before the containing one, even when
        // playback has already moved the playhead past a title start.
        assert_eq!(previous_title_index(&titles, 0, 30), None);
        assert_eq!(previous_title_index(&titles, 5, 30), None);
        assert_eq!(previous_title_index(&titles, 10, 30), Some(0));
        assert_eq!(previous_title_index(&titles, 15, 30), Some(0));
        assert_eq!(previous_title_index(&titles, 25, 30), Some(1));
        // At the end of the timeline, previous reaches the last title.
        assert_eq!(previous_title_index(&titles, 30, 30), Some(2));
        // Titles do not have to start at frame 0.
        let late = vec![marker(5, "A"), marker(15, "B")];
        assert_eq!(next_title_index(&late, 0, 30), Some(0));
        assert_eq!(previous_title_index(&late, 0, 30), None);
        assert_eq!(previous_title_index(&late, 5, 30), None);
        // Inside the last title: previous is the title before it.
        assert_eq!(previous_title_index(&late, 29, 30), Some(0));
        // At the end of the timeline: previous is the last title.
        assert_eq!(previous_title_index(&late, 30, 30), Some(1));
        assert_eq!(title_at_frame(&late, 4), None);
        assert_eq!(title_at_frame(&late, 5), Some(0));
        assert_eq!(title_at_frame(&late, 30), Some(1));
    }

    #[test]
    fn markers_snap_to_a_recent_title_jump() {
        let format = Info::test(44_100, 2);
        let titles = vec![marker(0, "A"), marker(100, "B"), marker(2000, "C")];
        let second = (f64::from(format.rate()) / format.samples() as f64).round() as usize;
        // Still inside the jumped-to title and within the snap window.
        assert_eq!(
            marker_frame(&titles, format, Some(100), 100 + second / 4),
            100
        );
        // Same title, but too long after the jump: no snap.
        assert_eq!(
            marker_frame(&titles, format, Some(100), 100 + 3 * second),
            100 + 3 * second
        );
        // Playback has moved into a later title: no snap.
        assert_eq!(marker_frame(&titles, format, Some(100), 2100), 2100);
        // No anchor: use the playhead.
        assert_eq!(marker_frame(&titles, format, None, 105), 105);
    }

    #[test]
    fn scrub_zoom_halves_and_doubles_within_limits() {
        let format = Info::test(44_100, 2);
        let max = frames_per_second(format);
        assert_eq!(max, 39);
        // Zooming in halves and stops at one frame.
        assert_eq!(zoomed_scrub_step(16, true), 8);
        assert_eq!(zoomed_scrub_step(2, true), 1);
        assert_eq!(zoomed_scrub_step(1, true), 1);
        // Zooming out doubles and stops at one second of frames.
        assert_eq!(zoomed_scrub_step(1, false), 2);
        assert_eq!(zoomed_scrub_step(32, false).min(max), max);
        assert_eq!(zoomed_scrub_step(max, false).min(max), max);
    }

    #[test]
    fn scrub_text_reports_frames_and_milliseconds() {
        let format = Info::test(44_100, 2);
        assert_eq!(scrub_text(1, format), "1 frame (26 ms)");
        assert_eq!(scrub_text(2, format), "2 frames (52 ms)");
        assert_eq!(
            scrub_text(frames_per_second(format), format),
            "39 frames (1019 ms)"
        );
    }

    #[test]
    fn scrub_snippet_is_about_a_tenth_of_a_second() {
        let format = Info::test(44_100, 2);
        assert_eq!(scrub_snap_frames(format), 4);
        // Slow formats still get at least one frame.
        assert_eq!(scrub_snap_frames(Info::test(8_000, 1)), 1);
    }

    #[test]
    fn speed_steps_cover_the_varispeed_range() {
        let steps: Vec<f32> = speed_steps().collect();
        assert_eq!(steps.len(), 16);
        assert_eq!(steps[0], 0.25);
        assert_eq!(steps[8], 2.25);
        assert_eq!(steps[15], 4.0);
    }

    #[test]
    fn cue_entries_use_derived_start_times() {
        let format = Info::test(44_100, 2);
        let frame = format.samples() as f64 / f64::from(format.rate());
        let titles = vec![marker(0, "A"), marker(10, "B")];
        let entries = cue_entries(&titles, &[], format, 20);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], (0.0, "A".into()));
        assert!((entries[1].0 - frame * 10.0).abs() < 1e-9);
        assert_eq!(entries[1].1, "B");
    }

    // ---------- file sources and the $file name token ----------

    #[test]
    fn source_at_prefers_the_span_starting_at_the_frame() {
        let sources = vec![source(0, "a"), source(50, "b")];
        assert_eq!(source_at(&sources, 0), Some("a"));
        assert_eq!(source_at(&sources, 49), Some("a"));
        // A cue point exactly at the boundary names the pasted file.
        assert_eq!(source_at(&sources, 50), Some("b"));
        assert_eq!(source_at(&sources, 500), Some("b"));
        assert_eq!(source_at(&[], 0), None);
    }

    #[test]
    fn spans_follow_inserts_deletes_and_replacements() {
        // Insert at the end: the pasted file's span starts where it lands.
        let mut sources = vec![source(0, "a")];
        splice_spans(&mut sources, 100, 100, 100, &[source(0, "b")], 50);
        assert_eq!(sources, vec![source(0, "a"), source(100, "b")]);

        // Insert at the start: the opened file's span moves past the block.
        let mut sources = vec![source(0, "a")];
        splice_spans(&mut sources, 100, 0, 0, &[source(0, "b")], 50);
        assert_eq!(sources, vec![source(0, "b"), source(50, "a")]);

        // Inserting audio of the same file merges into one span.
        let mut sources = vec![source(0, "a")];
        splice_spans(&mut sources, 100, 100, 100, &[source(0, "a")], 50);
        assert_eq!(sources, vec![source(0, "a")]);

        // A delete that crosses a boundary keeps both sides exact: frames
        // 40..50 were "a" and survive the delete of 40..60.
        let mut sources = vec![source(0, "a"), source(50, "b"), source(120, "a")];
        splice_spans(&mut sources, 150, 40, 60, &[], 0);
        assert_eq!(
            sources,
            vec![source(0, "a"), source(40, "b"), source(100, "a")]
        );

        // A paste that replaces audio with another file's frames.
        let mut sources = vec![source(0, "a")];
        splice_spans(&mut sources, 100, 20, 80, &[source(0, "c")], 10);
        assert_eq!(
            sources,
            vec![source(0, "a"), source(20, "c"), source(30, "a")]
        );

        // Replacing with the same file's audio merges back into one span.
        let mut sources = vec![source(0, "a")];
        splice_spans(&mut sources, 100, 20, 80, &[source(0, "a")], 10);
        assert_eq!(sources, vec![source(0, "a")]);

        // Deleting everything empties the spans.
        let mut sources = vec![source(0, "a"), source(50, "b")];
        splice_spans(&mut sources, 100, 0, 100, &[], 0);
        assert!(sources.is_empty());
    }

    #[test]
    fn sources_in_range_rebases_like_titles() {
        let sources = vec![source(0, "a"), source(10, "b"), source(20, "c")];
        assert_eq!(
            sources_in_range(&sources, &(5..25)),
            vec![source(0, "a"), source(5, "b"), source(15, "c")]
        );
        assert_eq!(sources_in_range(&sources, &(10..12)), vec![source(0, "b")]);
        assert!(sources_in_range(&sources, &(20..20)).is_empty());
    }

    #[test]
    fn file_token_expands_to_the_audio_s_file() {
        let sources = vec![source(0, "show"), source(50, "promo")];
        assert_eq!(
            expanded_title("Intro - $file", &sources, 10),
            "Intro - show"
        );
        // A cue point exactly at the boundary names the pasted file.
        assert_eq!(expanded_title("$file", &sources, 50), "promo");
        // Names without the token are untouched.
        assert_eq!(expanded_title("plain", &sources, 10), "plain");
        // A timeline without source information expands to an empty string.
        assert_eq!(expanded_title("$file", &[], 0), "");
    }

    #[test]
    fn selection_cue_uses_rebased_source() {
        let sources = vec![source(0, "first.mp3"), source(50, "second.mp3")];
        let titles = vec![marker(50, "$file")];
        let range = 50..100;
        let entries = cue_entries(
            &titles_in_range(&titles, &range),
            &sources_in_range(&sources, &range),
            Info::test(44_100, 2),
            range.len(),
        );
        assert_eq!(entries, vec![(0.0, "second".into())]);
    }

    #[test]
    fn same_named_sources_keep_distinct_spans() {
        let first = "Monday/Part 1.mp3";
        let second = "Tuesday/Part 1.mp3";
        let mut sources = vec![source(0, first)];
        splice_spans(&mut sources, 50, 50, 50, &[source(0, second)], 50);
        assert_eq!(sources.len(), 2);
        assert_eq!(source_at(&sources, 50), Some(second));
        assert_eq!(expanded_title("$file", &sources, 50), "Part 1");
    }

    #[test]
    fn source_names_drop_the_extension() {
        assert_eq!(source_name(Path::new("C:\\x\\take 1.mp3")), "take 1");
        assert_eq!(source_name(Path::new("take 1.mp3")), "take 1");
        assert_eq!(source_name(Path::new("noext")), "noext");
    }

    // ---------- the combine warnings ----------

    #[test]
    fn bitrate_differences_are_judged_at_whole_kbps() {
        assert!(!bitrates_differ(Some(128.0), Some(128.03)));
        assert!(bitrates_differ(Some(128.0), Some(128.5)));
        assert!(bitrates_differ(Some(128.0), Some(192.0)));
        assert!(!bitrates_differ(None, Some(128.0)));
        assert!(!bitrates_differ(None, None));
    }

    #[test]
    fn average_kbps_is_frame_bytes_over_duration() {
        let info = Info::test(44_100, 2);
        let frames: Frames = (0..100)
            .map(|_| Arc::new(Frame::Data(vec![0u8; 441].into_boxed_slice())))
            .collect();
        let kbps = codec::average_kbps(info, &frames).unwrap();
        // 100 frames of 441 bytes over 100 * 1152 samples at 44.1 kHz.
        let expected = 44_100.0 * 8.0 / 1000.0 / (100.0 * 1152.0 / 44_100.0);
        assert!((kbps - expected).abs() < 1e-9);
        assert!(codec::average_kbps(info, &Frames::new()).is_none());
    }

    #[test]
    fn long_marked_ranges_ask_before_a_paste_replaces_them() {
        let format = Info::test(44_100, 2);
        // Exactly ten seconds of frames (rounded down) is the ordinary
        // corrected-take edit; anything past ten seconds asks first.
        let exactly_ten = (REPLACE_ASK_SECONDS * f64::from(format.rate()) / format.samples() as f64)
            .floor() as usize;
        assert!(!replace_needs_confirm(exactly_ten, format));
        assert!(replace_needs_confirm(exactly_ten + 1, format));
    }

    #[test]
    fn boundary_ask_applies_only_where_a_cue_point_can_sit() {
        let titles = vec![marker(0, "A"), marker(100, "B")];
        // A boundary inside the audio, with cue points already present.
        assert!(boundary_ask_wanted(&titles, 50, 200));
        // A cue point already sits exactly there.
        assert!(!boundary_ask_wanted(&titles, 100, 200));
        // The boundary is the end of the timeline: no track would follow.
        assert!(!boundary_ask_wanted(&titles, 50, 50));
        // No cue points yet: dialog combining is never asked.
        assert!(!boundary_ask_wanted(&[], 50, 200));
    }

    #[test]
    fn jump_dialog_starts_at_the_playhead_or_the_title() {
        let format = Info::test(44_100, 2);
        let titles = vec![marker(0, "A"), marker(100, "B")];
        // At a title boundary: the t number.
        assert_eq!(jump_initial_text(&titles, format, None, 100), "t2");
        // Right after a jump to title B the anchor still answers t2, even
        // though the playhead has moved a few frames into the title.
        assert_eq!(jump_initial_text(&titles, format, Some(100), 105), "t2");
        // Mid-title with no anchor: the playhead's own time.
        assert_eq!(
            jump_initial_text(&titles, format, None, 105),
            super::time_text(format, 105)
        );
    }
}
