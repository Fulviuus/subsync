use crate::subtitle::Cue;
use anyhow::{Result, ensure};

pub const FRAME_MS: i64 = 10;
#[derive(Debug)]
pub struct Alignment {
    pub offset_ms: i64,
    pub score: f64,
    pub zero_score: f64,
    pub prominence: f64,
    pub sections: Vec<(i64, f64)>,
}

fn intervals(cues: &[Cue]) -> Vec<(i64, i64)> {
    let mut ranges: Vec<_> = cues
        .iter()
        .map(|c| (c.start_ms / FRAME_MS, (c.end_ms + FRAME_MS - 1) / FRAME_MS))
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
            continue;
        }
        merged.push((start, end));
    }
    merged
}

struct Signal {
    prefix: Vec<f64>,
}
impl Signal {
    fn score(&self, spans: &[(i64, i64)], shift: i64, window: (i64, i64)) -> f64 {
        let (lo, hi) = window;
        let n = (hi - lo) as f64;
        let audio = self.prefix[hi as usize] - self.prefix[lo as usize];
        let mut active = 0.0;
        let mut overlap = 0.0;
        for &(start, end) in spans {
            let a = (start + shift).clamp(lo, hi) as usize;
            let b = (end + shift).clamp(lo, hi) as usize;
            active += (b - a) as f64;
            overlap += self.prefix[b] - self.prefix[a];
        }
        let denom = (active * (1.0 - active / n) * audio * (1.0 - audio / n)).sqrt();
        if denom < 1e-6 {
            return 0.0;
        }
        (overlap - active * audio / n) / denom
    }

    fn search(&self, spans: &[(i64, i64)], max: i64, window: (i64, i64)) -> Vec<(i64, f64)> {
        (-max..=max)
            .map(|s| (s, self.score(spans, s, window)))
            .collect()
    }
}

pub fn estimate(speech: &[f64], cues: &[Cue], max_shift_ms: i64) -> Result<Alignment> {
    ensure!(
        cues.len() >= 10,
        "Need at least 10 subtitle cues for reliable alignment"
    );
    let mut prefix = Vec::with_capacity(speech.len() + 1);
    prefix.push(0.0);
    for value in speech {
        prefix.push(prefix.last().unwrap() + value);
    }
    let signal = Signal { prefix };
    let spans = intervals(cues);
    let window = (0, speech.len() as i64);
    let max = max_shift_ms / FRAME_MS;
    let scores = signal.search(&spans, max, window);
    let &(best, score) = scores
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.abs().cmp(&a.0.abs())))
        .unwrap();
    let competitor = scores
        .iter()
        .filter(|(s, _)| (s - best).abs() > 100)
        .map(|(_, s)| *s)
        .fold(-1.0, f64::max);
    let mut sections = Vec::new();
    // Check independent thirds: mismatched edits or frame-rate drift should
    // not be mistaken for one correct global shift.
    for part in 0..3 {
        let section = (window.1 * part / 3, window.1 * (part + 1) / 3);
        let section_cues: Vec<_> = cues
            .iter()
            .filter(|c| {
                let mid = (c.start_ms + c.end_ms) / 2 / FRAME_MS + best;
                mid >= section.0 && mid < section.1
            })
            .cloned()
            .collect();
        if section_cues.len() < 10 {
            continue;
        }
        let ranges = intervals(&section_cues);
        let local = signal.search(&ranges, max, section);
        let &(shift, confidence) = local.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        sections.push((shift * FRAME_MS, confidence));
    }
    Ok(Alignment {
        offset_ms: best * FRAME_MS,
        score,
        zero_score: scores[max as usize].1,
        prominence: score - competitor,
        sections,
    })
}

impl Alignment {
    pub fn validate(&self, max_shift_ms: i64) -> Result<()> {
        ensure!(
            self.offset_ms.abs() < max_shift_ms - 20,
            "Best shift hits search limit; try a larger --max-shift"
        );
        ensure!(
            self.score >= 0.12 && self.prominence >= 0.015,
            "Cannot confidently align subtitles (correlation {:.3}, peak separation {:.3}); original left untouched",
            self.score,
            self.prominence
        );
        let strong: Vec<_> = self
            .sections
            .iter()
            .filter(|(_, score)| *score >= 0.12)
            .collect();
        ensure!(
            strong.len() >= 2,
            "Not enough independently matching sections; original left untouched"
        );
        ensure!(
            strong
                .iter()
                .all(|(shift, _)| (*shift - self.offset_ms).abs() <= 750)
                && strong.iter().map(|(s, _)| *s).max().unwrap()
                    - strong.iter().map(|(s, _)| *s).min().unwrap()
                    <= 750,
            "Timing differs across the video (section shifts: {:?} ms). A single shift cannot safely fix different cuts or timing drift; original left untouched",
            strong.iter().map(|(s, _)| s).collect::<Vec<_>>()
        );
        Ok(())
    }
}

#[derive(Debug)]
pub struct Segment {
    pub first_cue: usize,
    pub end_cue: usize,
    pub offset_ms: i64,
    pub correlation: f64,
}

#[derive(Debug)]
pub struct Plan {
    pub offsets_ms: Vec<i64>,
    pub segments: Vec<Segment>,
}

/// ALASS proposes constant offsets separated by timing breaks. Independent
/// correlation searches verify each proposed segment before any file is touched.
pub fn plan(speech: &[f64], cues: &[Cue], max_shift_ms: i64, no_split: bool) -> Result<Plan> {
    use alass_core::{NoProgressHandler, TimePoint, TimeSpan, standard_scoring};
    ensure!(
        cues.len() >= 20,
        "Need at least 20 subtitle cues for reliable alignment"
    );
    let span = |a, b| TimeSpan::new(TimePoint::from(a), TimePoint::from(b));
    let mut reference = Vec::new();
    let mut start = None;
    for (i, value) in speech.iter().chain(std::iter::once(&0.0)).enumerate() {
        if *value > 0.0 {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(a) = start.take()
            && i - a >= 10
        {
            reference.push(span(a as i64, i as i64));
        }
    }
    ensure!(
        reference.len() >= 10,
        "Not enough speech detected; original left untouched"
    );
    let input: Vec<_> = cues
        .iter()
        .map(|c| span(c.start_ms / FRAME_MS, (c.end_ms + FRAME_MS - 1) / FRAME_MS))
        .collect();
    let offsets_ms: Vec<_> = if no_split {
        let (shift, _) =
            alass_core::align_nosplit(&reference, &input, standard_scoring, NoProgressHandler);
        vec![shift.as_i64() * FRAME_MS; cues.len()]
    } else {
        let (shifts, _) = alass_core::align(
            &reference,
            &input,
            10.0,
            Some(1.0),
            standard_scoring,
            NoProgressHandler,
        );
        shifts.iter().map(|d| d.as_i64() * FRAME_MS).collect()
    };
    ensure!(
        offsets_ms.iter().all(|s| s.abs() < max_shift_ms - 20),
        "Proposed alignment exceeds --max-shift; original left untouched"
    );
    let mut prefix = Vec::with_capacity(speech.len() + 1);
    prefix.push(0.0);
    for value in speech {
        prefix.push(prefix.last().unwrap() + value);
    }
    let signal = Signal { prefix };
    let mut segments = Vec::new();
    let mut first = 0;
    while first < cues.len() {
        let offset = offsets_ms[first];
        let end = (first + 1..cues.len())
            .find(|&i| offsets_ms[i] != offset)
            .unwrap_or(cues.len());
        ensure!(
            end - first >= 20,
            "Alignment needs a short, uncertain timing segment (cue {}); original left untouched",
            first + 1
        );
        let group = &cues[first..end];
        let lo = group.iter().map(|c| c.start_ms).min().unwrap() + offset;
        let hi = group.iter().map(|c| c.end_ms).max().unwrap() + offset;
        let window = (
            (lo / FRAME_MS - 100).max(0).min(speech.len() as i64),
            (hi / FRAME_MS + 100).clamp(0, speech.len() as i64),
        );
        ensure!(
            window.1 - window.0 >= 4500,
            "Not enough audio in timing segment; original left untouched"
        );
        let spans = intervals(group);
        let scores = signal.search(&spans, max_shift_ms / FRAME_MS, window);
        let &(best, score) = scores.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        let competitor = scores
            .iter()
            .filter(|(s, _)| (*s - best).abs() > 100)
            .map(|(_, s)| *s)
            .fold(-1.0, f64::max);
        let proposed = signal.score(&spans, offset / FRAME_MS, window);
        ensure!(
            score >= 0.12
                && score - competitor >= 0.015
                && (best * FRAME_MS - offset).abs() <= 750
                && score - proposed <= 0.035,
            "Uncertain alignment at cues {}–{} (correlation {:.3}, separation {:.3}); original left untouched",
            first + 1,
            end,
            proposed,
            score - competitor
        );
        // Verify the first and second halves separately, preventing a locally
        // good fit from hiding drift or an unrelated portion of the subtitle.
        for half in [(&group[..group.len() / 2]), (&group[group.len() / 2..])] {
            let ranges = intervals(half);
            let a = (ranges[0].0 + offset / FRAME_MS - 100).clamp(0, speech.len() as i64);
            let b =
                (ranges.last().unwrap().1 + offset / FRAME_MS + 100).clamp(0, speech.len() as i64);
            ensure!(b > a, "Timing segment lies outside audio");
            let local = signal.search(&ranges, max_shift_ms / FRAME_MS, (a, b));
            let &(shift, confidence) = local.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
            ensure!(
                confidence >= 0.10 && (shift * FRAME_MS - offset).abs() <= 750,
                "Timing varies within cues {}–{}; a reliable correction could not be established; original left untouched",
                first + 1,
                end
            );
        }
        segments.push(Segment {
            first_cue: first,
            end_cue: end,
            offset_ms: offset,
            correlation: proposed,
        });
        first = end;
    }
    // Prevent a timing break from creating a new overlap or reordering cues.
    let mut order: Vec<_> = (0..cues.len()).collect();
    order.sort_by_key(|&i| cues[i].start_ms);
    for pair in order.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        ensure!(
            cues[a].start_ms + offsets_ms[a] <= cues[b].start_ms + offsets_ms[b],
            "Alignment would reorder cues; original left untouched"
        );
        if cues[a].end_ms <= cues[b].start_ms {
            ensure!(
                cues[a].end_ms + offsets_ms[a] <= cues[b].start_ms + offsets_ms[b],
                "Alignment would introduce overlapping cues; original left untouched"
            );
        }
    }
    Ok(Plan {
        offsets_ms,
        segments,
    })
}
