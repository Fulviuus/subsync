use anyhow::{Context, Result, bail, ensure};
use regex::bytes::Regex;
use std::{
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

pub const EXTENSIONS: &[&str] = &["srt", "vtt", "ass", "ssa"];
const MAX_TIME_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct Cue {
    pub start_ms: i64,
    pub end_ms: i64,
    start: Range<usize>,
    end: Range<usize>,
    style: Style,
}

#[derive(Clone, Copy, Debug)]
enum Style {
    Srt,
    Vtt(bool),
    Ass,
}

pub struct Subtitle {
    pub original: Vec<u8>,
    pub cues: Vec<Cue>,
}

pub fn discover(video: &Path) -> Result<PathBuf> {
    let stem = video.file_stem().context("Video has no filename")?;
    let folder = video
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut matches = Vec::new();
    for entry in fs::read_dir(folder).context("Cannot read video folder")? {
        let path = entry?.path();
        if path.file_stem() == Some(stem)
            && path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
        {
            matches.push(path);
        }
    }
    matches.sort();
    match matches.len() {
        0 => bail!(
            "No matching subtitle found for {} (expected same filename with .srt, .vtt, .ass or .ssa)",
            video.display()
        ),
        1 => Ok(matches.remove(0)),
        _ => bail!(
            "Multiple matching subtitles: {}. Choose one with --subtitle <file>",
            matches
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn parse_time(bytes: &[u8], ass: bool) -> Result<i64> {
    let s = std::str::from_utf8(bytes)?;
    let parts: Vec<_> = s.split([':', ',', '.']).collect();
    ensure!(
        parts.len() == 3 || parts.len() == 4,
        "Invalid timestamp: {s}"
    );
    let nums: Vec<i64> = parts
        .iter()
        .map(|s| s.parse())
        .collect::<std::result::Result<_, _>>()?;
    let (h, m, sec, frac) = if nums.len() == 4 {
        (nums[0], nums[1], nums[2], nums[3])
    } else {
        (0, nums[0], nums[1], nums[2])
    };
    ensure!(
        m < 60 && sec < 60 && frac < if ass { 100 } else { 1000 },
        "Invalid timestamp: {s}"
    );
    let ms = ((h * 60 + m) * 60 + sec) * 1000 + frac * if ass { 10 } else { 1 };
    ensure!(ms <= MAX_TIME_MS, "Timestamp exceeds seven days: {s}");
    Ok(ms)
}

impl Subtitle {
    pub fn parse(original: Vec<u8>, extension: &str) -> Result<Self> {
        ensure!(
            !original.contains(&0),
            "UTF-16/binary subtitles are unsupported; convert to UTF-8 first"
        );
        let ext = extension.to_ascii_lowercase();
        if ext == "vtt" {
            ensure!(
                !original.windows(15).any(|w| w == b"X-TIMESTAMP-MAP"),
                "Mapped WebVTT timestamps are unsupported; use a standalone subtitle file"
            );
            let inline = Regex::new(r"<(?:[0-9]+:)?[0-9]{2}:[0-9]{2}\.[0-9]{3}>")?;
            ensure!(
                !inline.is_match(&original),
                "WebVTT inline karaoke timestamps are unsupported; original left untouched"
            );
        }
        let ass = matches!(ext.as_str(), "ass" | "ssa");
        ensure!(
            ass || matches!(ext.as_str(), "srt" | "vtt"),
            "Unsupported subtitle format: {extension}"
        );
        let timeline = Regex::new(
            r"^[ \t]*((?:[0-9]{1,5}:)?[0-9]{2}:[0-9]{2}[,.][0-9]{3})[ \t]+-->[ \t]+((?:[0-9]{1,5}:)?[0-9]{2}:[0-9]{2}[,.][0-9]{3})(?:[ \t].*)?$",
        )?;
        let ass_time = Regex::new(r"^[0-9]{1,5}:[0-9]{2}:[0-9]{2}\.[0-9]{2}$")?;
        let mut cues = Vec::new();
        let mut pos = 0;
        let mut events = false;
        let mut columns: Option<Vec<String>> = None;
        for raw_line in original.split_inclusive(|&b| b == b'\n') {
            let raw_end = raw_line.len();
            let mut line = raw_line;
            while line.last().is_some_and(|b| matches!(b, b'\r' | b'\n')) {
                line = &line[..line.len() - 1];
            }
            let bom = if pos == 0 && line.starts_with(b"\xef\xbb\xbf") {
                3
            } else {
                0
            };
            line = &line[bom..];
            let base = pos + bom;
            if ass {
                if line.starts_with(b"[") {
                    events = line.eq_ignore_ascii_case(b"[Events]");
                    columns = None;
                }
                if events && line.starts_with(b"Format:") {
                    columns = Some(
                        std::str::from_utf8(&line[7..])?
                            .split(',')
                            .map(|s| s.trim().to_ascii_lowercase())
                            .collect(),
                    );
                }
                if events && line.starts_with(b"Dialogue:") {
                    let cols = columns
                        .as_ref()
                        .context("ASS/SSA Events section needs a Format line")?;
                    ensure!(
                        cols.last().is_some_and(|s| s == "text"),
                        "ASS Text must be the last event field"
                    );
                    let start_index = cols
                        .iter()
                        .position(|s| s == "start")
                        .context("Missing ASS Start field")?;
                    let end_index = cols
                        .iter()
                        .position(|s| s == "end")
                        .context("Missing ASS End field")?;
                    let mut cursor = 9;
                    let fields: Vec<_> = line[9..]
                        .splitn(cols.len(), |&b| b == b',')
                        .map(|field| {
                            let leading =
                                field.iter().take_while(|b| b.is_ascii_whitespace()).count();
                            let trailing = field
                                .iter()
                                .rev()
                                .take_while(|b| b.is_ascii_whitespace())
                                .count();
                            let range = cursor + leading
                                ..cursor + field.len().saturating_sub(trailing).max(leading);
                            cursor += field.len() + 1;
                            range
                        })
                        .collect();
                    ensure!(fields.len() == cols.len(), "Malformed ASS Dialogue line");
                    let s = fields[start_index].clone();
                    let e = fields[end_index].clone();
                    ensure!(
                        ass_time.is_match(&line[s.clone()]) && ass_time.is_match(&line[e.clone()]),
                        "Invalid ASS timestamp"
                    );
                    cues.push(Cue {
                        start_ms: parse_time(&line[s.clone()], true)?,
                        end_ms: parse_time(&line[e.clone()], true)?,
                        start: base + s.start..base + s.end,
                        end: base + e.start..base + e.end,
                        style: Style::Ass,
                    });
                }
            } else if let Some(caps) = timeline.captures(line) {
                let s = caps.get(1).unwrap();
                let e = caps.get(2).unwrap();
                let style = if ext == "srt" {
                    Style::Srt
                } else {
                    Style::Vtt(s.as_bytes().iter().filter(|&&b| b == b':').count() == 2)
                };
                cues.push(Cue {
                    start_ms: parse_time(s.as_bytes(), false)?,
                    end_ms: parse_time(e.as_bytes(), false)?,
                    start: base + s.start()..base + s.end(),
                    end: base + e.start()..base + e.end(),
                    style,
                });
            } else if line.windows(3).any(|w| w == b"-->") {
                bail!("Malformed subtitle timing line near byte {base}");
            }
            pos += raw_end;
        }
        ensure!(!cues.is_empty(), "Subtitle contains no timed cues");
        for cue in &cues {
            ensure!(
                cue.end_ms > cue.start_ms,
                "Subtitle cue ends before or at its start"
            );
        }
        Ok(Self { original, cues })
    }

    pub fn shifted(&self, offset_ms: i64) -> Result<Vec<u8>> {
        self.shifted_by(&vec![offset_ms; self.cues.len()])
    }

    pub fn shifted_by(&self, offsets_ms: &[i64]) -> Result<Vec<u8>> {
        ensure!(
            offsets_ms.len() == self.cues.len(),
            "One shift is required for every cue"
        );
        let mut replacements = Vec::new();
        for (cue, &offset_ms) in self.cues.iter().zip(offsets_ms) {
            let start = cue
                .start_ms
                .checked_add(offset_ms)
                .context("Shift overflow")?
                .max(0);
            let end = cue
                .end_ms
                .checked_add(offset_ms)
                .context("Shift overflow")?
                .max(0);
            ensure!(
                end > start,
                "Shift would move an entire cue before time zero; original left untouched"
            );
            ensure!(end <= MAX_TIME_MS, "Shifted timestamp exceeds seven days");
            replacements.push((cue.start.clone(), format_time(start, cue.style)));
            replacements.push((cue.end.clone(), format_time(end, cue.style)));
        }
        replacements.sort_by_key(|(r, _)| r.start);
        let mut result = Vec::with_capacity(self.original.len());
        let mut cursor = 0;
        for (range, time) in replacements {
            result.extend_from_slice(&self.original[cursor..range.start]);
            result.extend_from_slice(time.as_bytes());
            cursor = range.end;
        }
        result.extend_from_slice(&self.original[cursor..]);
        Ok(result)
    }
}

fn format_time(ms: i64, style: Style) -> String {
    let ms = if matches!(style, Style::Ass) {
        (ms + 5) / 10 * 10
    } else {
        ms
    };
    let (h, m, s, f) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    match style {
        Style::Ass => format!("{h}:{m:02}:{s:02}.{:02}", f / 10),
        Style::Srt => format!("{h:02}:{m:02}:{s:02},{f:03}"),
        Style::Vtt(hours) => {
            if hours || h > 0 {
                format!("{h:02}:{m:02}:{s:02}.{f:03}")
            } else {
                format!("{m:02}:{s:02}.{f:03}")
            }
        }
    }
}
