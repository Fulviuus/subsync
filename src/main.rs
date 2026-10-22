use anyhow::{Context, Result, ensure};
use clap::Parser;
use std::{fs, path::PathBuf, process::ExitCode};
use subsync::{
    align, audio, storage,
    subtitle::{self, Subtitle},
};

#[derive(Parser)]
#[command(
    version,
    about = "Synchronize matching subtitles to video speech, backing up before replacement"
)]
struct Args {
    /// Video file; discovers a same-stem .srt, .vtt, .ass or .ssa beside it
    video: PathBuf,
    /// Explicit subtitle file (also resolves multiple matching formats)
    #[arg(long)]
    subtitle: Option<PathBuf>,
    /// Analyze and report without changing any files
    #[arg(long, alias = "check")]
    dry_run: bool,
    /// Largest absolute shift to search, in seconds (0.1–3600)
    #[arg(long, default_value_t = 120.0)]
    max_shift: f64,
    /// Offsets at or below this many milliseconds count as synchronized
    #[arg(long, default_value_t = 100)]
    tolerance_ms: u64,
    /// Audio stream index, starting at zero
    #[arg(long, default_value_t = 0)]
    audio_track: usize,
    /// Allow only a single constant shift (reject timing breaks)
    #[arg(long)]
    no_split: bool,
    /// Print whole-video and section correlation diagnostics
    #[arg(long, short)]
    verbose: bool,
}

fn run(args: Args) -> Result<()> {
    ensure!(
        args.max_shift.is_finite() && (0.1..=3600.0).contains(&args.max_shift),
        "--max-shift must be between 0.1 and 3600 seconds"
    );
    ensure!(
        args.tolerance_ms <= 1000,
        "--tolerance-ms must be at most 1000"
    );
    ensure!(
        args.video.is_file(),
        "Video file not found: {}",
        args.video.display()
    );
    let path = match args.subtitle {
        Some(path) => path,
        None => subtitle::discover(&args.video)?,
    };
    ensure!(
        fs::symlink_metadata(&path)?.file_type().is_file(),
        "Subtitle must be a regular file, not a symbolic link"
    );
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .context("Subtitle has no extension")?;
    let subs = Subtitle::parse(fs::read(&path).context("Cannot read subtitle")?, ext)?;
    println!("Subtitle: {} ({} cues)", path.display(), subs.cues.len());
    eprintln!("Analyzing audio speech locally…");
    let speech = audio::speech(&args.video, args.audio_track)?;
    let max_ms = (args.max_shift * 1000.0).round() as i64;
    if args.verbose {
        let alignment = align::estimate(&speech, &subs.cues, max_ms)?;
        println!(
            "Best single shift: {:+.3} s\nCorrelation: {:.3} → {:.3}; peak separation: {:.3}",
            alignment.offset_ms as f64 / 1000.0,
            alignment.zero_score,
            alignment.score,
            alignment.prominence
        );
        println!(
            "Section estimates: {}",
            alignment
                .sections
                .iter()
                .map(|(s, c)| format!("{:+.3} s ({c:.3})", *s as f64 / 1000.0))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let plan = align::plan(&speech, &subs.cues, max_ms, args.no_split)?;
    for segment in &plan.segments {
        println!(
            "Cues {}–{}: {:+.3} s (correlation {:.3})",
            segment.first_cue + 1,
            segment.end_cue,
            segment.offset_ms as f64 / 1000.0,
            segment.correlation
        );
    }
    if plan
        .offsets_ms
        .iter()
        .all(|s| s.unsigned_abs() <= args.tolerance_ms)
    {
        println!(
            "Already synchronized within {} ms; file unchanged.",
            args.tolerance_ms
        );
        return Ok(());
    }
    let updated = subs.shifted_by(&plan.offsets_ms)?;
    if args.dry_run {
        println!(
            "Dry run: would back up and synchronize {} in {} timing segment(s).",
            path.display(),
            plan.segments.len()
        );
        return Ok(());
    }
    let backup = storage::replace_with_backup(&path, &subs.original, &updated)?;
    println!(
        "Backup: {}\nSynchronized: {}",
        backup.display(),
        path.display()
    );
    Ok(())
}
fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
