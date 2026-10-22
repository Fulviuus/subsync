use std::fs;
use subsync::{
    align, storage,
    subtitle::{self, Subtitle},
};

fn srt(text: &[u8]) -> Subtitle {
    Subtitle::parse(text.to_vec(), "srt").unwrap()
}

#[test]
fn srt_preserves_bom_crlf_text_and_legacy_encoding() {
    let input = b"\xef\xbb\xbf1\r\n00:00:01,250 --> 00:00:02,500\r\n<i>caf\xe9</i>\r\n\r\n";
    let parsed = srt(input);
    assert_eq!(
        parsed.shifted(750).unwrap(),
        b"\xef\xbb\xbf1\r\n00:00:02,000 --> 00:00:03,250\r\n<i>caf\xe9</i>\r\n\r\n"
    );
}

#[test]
fn vtt_preserves_settings_and_cue_identifiers() {
    let parsed = Subtitle::parse(
        b"WEBVTT\n\nintro\n00:59.500 --> 01:01.000 align:start position:10%\nHello\n".to_vec(),
        "vtt",
    )
    .unwrap();
    assert_eq!(
        parsed.shifted(3_600_000).unwrap(),
        b"WEBVTT\n\nintro\n01:00:59.500 --> 01:01:01.000 align:start position:10%\nHello\n"
    );
}

#[test]
fn ass_only_updates_dialogue_preserving_styles_and_commas() {
    for ext in ["ass", "ssa"] {
        let input = b"[Script Info]\nTitle: sample\n[Events]\nFormat: Layer, End, Start, Style, Text\nComment: 0,0:00:02.00,0:00:01.00,Default,Keep\nDialogue: 0,0:00:02.00,0:00:01.00,Default,{\\i1}Hello, world\n";
        let parsed = Subtitle::parse(input.to_vec(), ext).unwrap();
        let output = String::from_utf8(parsed.shifted(1230).unwrap()).unwrap();
        assert!(output.contains("Dialogue: 0,0:00:03.23,0:00:02.23,Default,{\\i1}Hello, world"));
        assert!(output.contains("Comment: 0,0:00:02.00,0:00:01.00,Default,Keep"));
    }
}

#[test]
fn invalid_and_negative_timestamps_are_safe() {
    for input in [
        "1\n00:61:01,000 --> 00:62:02,000\na\n",
        "1\n00:00:02,000 --> 00:00:01,000\na\n",
        "1\nnot-a-time --> 00:00:02,000\na\n",
        "empty",
        "\0\0",
    ] {
        assert!(Subtitle::parse(input.as_bytes().to_vec(), "srt").is_err());
    }
    let parsed = srt(b"1\n00:00:01,000 --> 00:00:02,000\nHello\n");
    assert!(parsed.shifted(-2500).is_err());
    assert!(
        String::from_utf8(parsed.shifted(-1500).unwrap())
            .unwrap()
            .contains("00:00:00,000 --> 00:00:00,500")
    );
}

#[test]
fn discovery_requires_exact_stem_and_rejects_ambiguity() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("Episode.mkv");
    fs::write(dir.path().join("Episode.en.srt"), b"x").unwrap();
    assert!(subtitle::discover(&video).is_err());
    let expected = dir.path().join("Episode.SRT");
    fs::write(&expected, b"x").unwrap();
    assert_eq!(subtitle::discover(&video).unwrap(), expected);
    fs::write(dir.path().join("Episode.ass"), b"x").unwrap();
    assert!(subtitle::discover(&video).is_err());
}

#[test]
fn replacement_preserves_backups_and_detects_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("episode.srt");
    fs::write(&path, b"original").unwrap();
    let first = storage::replace_with_backup(&path, b"original", b"shifted").unwrap();
    assert_eq!(fs::read(&first).unwrap(), b"original");
    assert_eq!(fs::read(&path).unwrap(), b"shifted");
    let second = storage::replace_with_backup(&path, b"shifted", b"shifted again").unwrap();
    assert_ne!(first, second);
    assert_eq!(fs::read(&first).unwrap(), b"original");
    assert_eq!(fs::read(&second).unwrap(), b"shifted");
    assert!(storage::replace_with_backup(&path, b"stale", b"oops").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"shifted again");
}

#[cfg(unix)]
#[test]
fn replacement_rejects_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.srt");
    let link = dir.path().join("link.srt");
    fs::write(&target, b"original").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(storage::replace_with_backup(&link, b"original", b"oops").is_err());
    assert_eq!(fs::read(target).unwrap(), b"original");
}

// Deterministic, irregular speech gaps: independently generated test signal,
// with many cues across all thirds and no periodic alternative alignments.
fn fixture(offset_ms: i64) -> (Vec<f64>, Subtitle) {
    let mut speech = vec![0.0; 60000];
    let mut text = String::new();
    let mut state = 42u64;
    let mut cursor = 2000usize;
    for i in 0..100 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        cursor += 120 + (state % 300) as usize;
        let length = 60 + (state / 1000 % 180) as usize;
        speech[cursor..cursor + length].fill(1.0);
        let stamp = |frame: usize| {
            let ms = frame as i64 * 10 + offset_ms;
            format!(
                "{:02}:{:02}:{:02},{:03}",
                ms / 3600000,
                ms / 60000 % 60,
                ms / 1000 % 60,
                ms % 1000
            )
        };
        text.push_str(&format!(
            "{}\n{} --> {}\nText\n\n",
            i + 1,
            stamp(cursor),
            stamp(cursor + length)
        ));
        cursor += length;
    }
    (speech, srt(text.as_bytes()))
}

#[test]
fn recovers_early_late_and_already_synced_subtitles() {
    for offset in [-7350, 0, 5230] {
        let (speech, subs) = fixture(offset);
        let result = align::estimate(&speech, &subs.cues, 15000).unwrap();
        assert_eq!(result.offset_ms, -offset);
        result.validate(15000).unwrap();
    }
}

#[test]
fn silence_noise_and_out_of_range_are_rejected() {
    let (mut speech, subs) = fixture(5000);
    let limited = align::estimate(&speech, &subs.cues, 2000).unwrap();
    assert!(limited.validate(2000).is_err(), "{limited:?}");
    speech.fill(0.0);
    assert!(
        align::estimate(&speech, &subs.cues, 15000)
            .unwrap()
            .validate(15000)
            .is_err()
    );
    speech.fill(1.0);
    assert!(
        align::estimate(&speech, &subs.cues, 15000)
            .unwrap()
            .validate(15000)
            .is_err()
    );
}

#[test]
fn inconsistent_cuts_are_rejected() {
    let (speech, mut subs) = fixture(0);
    for cue in &mut subs.cues {
        if cue.start_ms > 200_000 {
            cue.start_ms += 3000;
            cue.end_ms += 3000;
        }
    }
    let result = align::estimate(&speech, &subs.cues, 15000).unwrap();
    assert!(result.validate(15000).is_err());
}

#[test]
fn validated_plan_handles_constant_shifts_and_is_idempotent() {
    for offset in [-7350, 0, 5230] {
        let (speech, subs) = fixture(offset);
        let plan = align::plan(&speech, &subs.cues, 15000, false).unwrap();
        assert!(plan.offsets_ms.iter().all(|&s| s == -offset));
        let corrected = srt(&subs.shifted_by(&plan.offsets_ms).unwrap());
        let again = align::plan(&speech, &corrected.cues, 15000, false).unwrap();
        assert!(again.offsets_ms.iter().all(|&s| s == 0));
    }
}

#[test]
fn validated_plan_handles_timing_breaks() {
    let (speech, mut subs) = fixture(1000);
    for cue in &mut subs.cues {
        if cue.start_ms > 200_000 {
            cue.start_ms += 3000;
            cue.end_ms += 3000;
        }
    }
    let plan = align::plan(&speech, &subs.cues, 15000, false).unwrap();
    assert_eq!(plan.segments.len(), 2);
    for (cue, shift) in subs.cues.iter().zip(plan.offsets_ms) {
        assert_eq!(shift, if cue.start_ms > 200_000 { -4000 } else { -1000 });
    }
    assert!(align::plan(&speech, &subs.cues, 15000, true).is_err());
}

#[test]
fn validated_plan_rejects_silence_wrong_audio_and_out_of_range() {
    let (mut speech, subs) = fixture(5000);
    assert!(align::plan(&speech, &subs.cues, 2000, false).is_err());
    speech.fill(0.0);
    assert!(align::plan(&speech, &subs.cues, 15000, false).is_err());
    speech.fill(1.0);
    assert!(align::plan(&speech, &subs.cues, 15000, false).is_err());
    let mut seed = 123u64;
    for frame in &mut speech {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        *frame = if seed >> 63 == 0 { 0.0 } else { 1.0 };
    }
    assert!(align::plan(&speech, &subs.cues, 15000, false).is_err());
}

#[test]
fn vtt_rejects_timestamps_that_cannot_be_safely_shifted() {
    for body in [
        "WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:900000\n\n00:01.000 --> 00:02.000\nHello\n",
        "WEBVTT\n\n00:01.000 --> 00:02.000\nHello <00:01.500>world\n",
    ] {
        assert!(Subtitle::parse(body.as_bytes().to_vec(), "vtt").is_err());
    }
}
