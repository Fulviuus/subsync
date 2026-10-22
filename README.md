# subsync

A Rust command that aligns an external subtitle file to the speech in a video:

```sh
subsync "/path/to/episode.mkv"
```

It finds a subtitle in the same folder with exactly the same filename stem, analyzes the audio locally, and corrects subtitle timing. Before changing anything it saves the original bytes to `episode.srt.bak`, then atomically replaces `episode.srt`. Existing backups are never overwritten: subsequent backups use `.bak.1`, `.bak.2`, etc.

Supported text formats: **SRT, WebVTT (.vtt), ASS and SSA**. Text, styling, cue identifiers, line endings, UTF-8 BOMs, and existing single-byte text encodings are preserved; only timestamps change. Missing subtitles, ambiguous matches, malformed input, failed audio decoding, and uncertain alignment produce an error and a nonzero exit status. Already aligned files are left unchanged, without creating a backup.

## Install

Requirements: Rust 1.88 or newer, a C compiler (for the bundled WebRTC speech detector), and `ffmpeg` on PATH. No Python, model download, API key, GPU, or cloud service is needed.

On macOS:

```sh
brew install ffmpeg
git clone git@github.com:Fulviuus/subsync.git
cd subsync
cargo install --path . --locked
```

Alternatively, install directly from GitHub with an account that has repository access:

```sh
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --git ssh://git@github.com/Fulviuus/subsync.git --locked
```

The executable is installed into `~/.cargo/bin`; ensure that directory is on PATH. On Debian/Ubuntu, install `ffmpeg` and `build-essential`, then use the same Cargo command with a recent Rust toolchain.

## Usage

```sh
# Analyze and fix a matching subtitle, backing it up first
subsync "episode.mkv"

# Preview proposed offsets without changing the subtitle
subsync "episode.mkv" --dry-run

# Choose a particular file, e.g. a language-suffixed subtitle
subsync "episode.mkv" --subtitle "episode.en.srt"

# Select the second audio track (indices start at zero)
subsync "episode.mkv" --audio-track 1

# Allow an offset up to five minutes in either direction
subsync "episode.mkv" --max-shift 300

# Require one constant shift, rejecting timing breaks
subsync "episode.mkv" --no-split

# Include correlation diagnostics
subsync "episode.mkv" --dry-run --verbose
```

Automatic discovery accepts `episode.srt`, `episode.vtt`, `episode.ass`, or `episode.ssa` beside `episode.mkv`. Extensions are case-insensitive; stems must match exactly. Multiple matches require `--subtitle`. Bitmap subtitles such as PGS/VobSub and embedded tracks are not edited.

Positive offsets move captions later; negative offsets move them earlier. The default maximum shift is 120 seconds, and the default already-synchronized tolerance is 100 ms (`--tolerance-ms`). The program reports cue ranges if different parts require different offsets.

## How alignment works

FFmpeg decodes the selected audio track to temporary 8 kHz mono PCM. WebRTC voice activity detection produces a speech/silence timeline in 10 ms frames. The Rust [ALASS library](https://github.com/kaegi/alass) proposes offsets, allowing timing breaks such as removed commercial gaps. Independent correlation searches check each proposed segment and its two halves before permitting a write. A C compiler is only required when building, not when running the installed executable.

Synchronization is an estimate based on speech timing, **not a guarantee of perfect word-level alignment**. It works with translated subtitles because it does not compare words. Music, sparse dialogue, the wrong audio track, substantial frame-rate drift, different edits, or unrelated subtitles can prevent reliable matching. The program requires at least 20 cues and 45 seconds per proposed segment, refuses weak or inconsistent matches, and does not provide a force-write switch. These confidence thresholds are heuristics, not statistical probabilities.

Negative start times are clipped to zero only if the cue still has positive duration. If an entire cue would move before zero, the operation fails without replacing the original. Newly introduced overlaps and cue reordering are rejected. Symbolic-link subtitle files are rejected; use a regular file. UTF-16 must first be converted to UTF-8. WebVTT timestamp maps and inline karaoke timestamps are explicitly unsupported. ASS/SSA requires an Events Format line with Text as its last field.

The temporary decoded audio uses about 58 MB per hour and is deleted on exit. Backup and replacement files are flushed before the atomic replacement. A final content check rejects edits made to the subtitle during analysis; avoid concurrent writers during replacement.

## Development and verification

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests cover early/late/already aligned subtitles, timing breaks, unrelated and silent audio signals, malformed timestamps, format preservation, discovery, safe replacement, decoder failures, and numbered backups.

An optional end-to-end test uses your own video and matching SRT. It operates on temporary subtitle copies, checks dry-run behavior, backup contents, repeated-run stability, and recovery from an artificial five-second delay:

```sh
SUBSYNC_TEST_VIDEO="/absolute/path/to/episode.mkv" \
  cargo test --release --test cli -- --include-ignored --nocapture
```

The fixture should initially be out of sync. No video, audio or subtitle content is included in this repository.

Verified on the provided Scrubs S02E05 sample (492 cues): the estimated correction was **−1.240 s for cues 1–279** and **−3.810 s for cues 280–492**. A second run made no changes. An additional five-second delay was recovered to within 40 ms. This verifies algorithm behavior and file safety; it is not a manual review of every line. The original Downloads subtitle was left untouched.

## Dependencies and licensing

This project is distributed under GPL-3.0, matching its `alass-core` dependency. WebRTC VAD is bundled through `webrtc-vad`; its Rust wrapper and native libfvad retain their upstream license notices. FFmpeg is a separately installed executable. See [LICENSE](LICENSE) and the dependency sources for license terms.
