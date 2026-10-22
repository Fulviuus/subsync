use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};
use subsync::subtitle::Subtitle;

fn run(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_subsync"))
        .args(args)
        .output()
        .unwrap()
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn missing_subtitles_fail_before_audio_decode() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("test.mkv");
    fs::write(&video, b"not a video").unwrap();
    let result = run(&[video.as_os_str()]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("No matching subtitle"));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn malformed_subtitles_fail_without_modification() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("test.mkv");
    let sub = video.with_extension("srt");
    fs::write(&video, b"not a video").unwrap();
    fs::write(&sub, b"not a subtitle").unwrap();
    let result = run(&[video.as_os_str()]);
    assert!(!result.status.success());
    assert_eq!(fs::read(&sub).unwrap(), b"not a subtitle");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn failed_decoder_preserves_original() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("test.mkv");
    let sub = video.with_extension("srt");
    let original = b"1\n00:00:01,000 --> 00:00:03,000\nhello\n";
    fs::write(&video, b"not a video").unwrap();
    fs::write(&sub, original).unwrap();
    let result = run(&[video.as_os_str()]);
    assert!(!result.status.success());
    assert_eq!(fs::read(&sub).unwrap(), original);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
}

/// Use a local, legally obtained video with its same-stem SRT. No media is
/// checked into the repository, and this test only modifies temporary copies.
#[test]
#[ignore = "requires SUBSYNC_TEST_VIDEO and FFmpeg"]
fn real_video_backup_dry_run_idempotence_and_known_delay() {
    let video =
        PathBuf::from(std::env::var_os("SUBSYNC_TEST_VIDEO").expect("Set SUBSYNC_TEST_VIDEO"));
    let original = fs::read(video.with_extension("srt")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("episode.srt");
    fs::write(&copy, &original).unwrap();
    let args = [
        video.as_os_str(),
        std::ffi::OsStr::new("--subtitle"),
        copy.as_os_str(),
    ];
    let mut dry = args.to_vec();
    dry.push(std::ffi::OsStr::new("--dry-run"));
    println!("{}", success(run(&dry)));
    assert_eq!(fs::read(&copy).unwrap(), original);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    println!("{}", success(run(&args)));
    let corrected = fs::read(&copy).unwrap();
    assert_ne!(
        corrected, original,
        "Test media should initially be out of sync"
    );
    assert_eq!(
        fs::read(dir.path().join("episode.srt.bak")).unwrap(),
        original
    );
    let again = success(run(&args));
    println!("{again}");
    assert!(again.contains("Already synchronized"));
    assert_eq!(fs::read(&copy).unwrap(), corrected);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    let parsed = Subtitle::parse(corrected.clone(), "srt").unwrap();
    fs::write(&copy, parsed.shifted(5000).unwrap()).unwrap();
    println!("{}", success(run(&args)));
    let repaired = Subtitle::parse(fs::read(&copy).unwrap(), "srt").unwrap();
    assert_eq!(parsed.cues.len(), repaired.cues.len());
    for (a, b) in parsed.cues.iter().zip(repaired.cues) {
        assert!((a.start_ms - b.start_ms).abs() <= 100);
        assert!((a.end_ms - b.end_ms).abs() <= 100);
    }
    assert!(dir.path().join("episode.srt.bak.1").exists());
    assert_eq!(
        fs::read(dir.path().join("episode.srt.bak")).unwrap(),
        original
    );
}
