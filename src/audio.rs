use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::File,
    io::{BufReader, Read, Seek},
    path::Path,
    process::{Command, Stdio},
};
use webrtc_vad::{SampleRate, Vad, VadMode};

/// One frame is 10 ms, at 8 kHz. Decode on disk so a long film does not
/// require keeping all PCM samples in memory. No audio leaves this machine.
pub fn speech(video: &Path, track: usize) -> Result<Vec<f64>> {
    let mut pcm = tempfile::tempfile().context("Cannot create temporary audio file")?;
    let errors = tempfile::tempfile()?;
    let status = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-copyts",
            "-start_at_zero",
            "-i",
        ])
        .arg(video)
        .args([
            "-map",
            &format!("0:a:{track}"),
            "-vn",
            "-af",
            "aresample=8000:async=1:first_pts=0",
            "-ac",
            "1",
            "-ar",
            "8000",
            "-f",
            "s16le",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(pcm.try_clone()?))
        .stderr(Stdio::from(errors.try_clone()?))
        .status()
        .context("Cannot run ffmpeg. Install FFmpeg and make sure it is on PATH")?;
    if !status.success() {
        bail!("FFmpeg could not decode audio: {}", read_errors(errors)?);
    }
    pcm.rewind()?;
    let mut reader = BufReader::new(pcm);
    let mut vad = Vad::new_with_rate_and_mode(SampleRate::Rate8kHz, VadMode::Aggressive);
    let mut bytes = [0u8; 160];
    let mut samples = [0i16; 80];
    let mut speech = Vec::new();
    loop {
        match reader.read_exact(&mut bytes) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        for (sample, pair) in samples.iter_mut().zip(bytes.chunks_exact(2)) {
            *sample = i16::from_le_bytes([pair[0], pair[1]]);
        }
        speech.push(
            if vad
                .is_voice_segment(&samples)
                .map_err(|_| anyhow::anyhow!("Voice detector rejected audio frame"))?
            {
                1.0
            } else {
                0.0
            },
        );
    }
    ensure!(
        speech.len() >= 1000,
        "Need at least 10 seconds of audio to estimate alignment"
    );
    Ok(speech)
}

fn read_errors(mut file: File) -> Result<String> {
    file.rewind()?;
    let mut result = String::new();
    file.take(8192).read_to_string(&mut result)?;
    Ok(result.trim().to_owned())
}
