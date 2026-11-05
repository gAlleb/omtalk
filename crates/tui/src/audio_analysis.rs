//! Reuse waveform analysis across history pages and application restarts.
use super::*;
use std::hash::{Hash, Hasher};

#[derive(serde::Serialize, serde::Deserialize, PartialEq)]
struct Fingerprint {
    path: PathBuf,
    bytes: u64,
    modified: u128,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Cached {
    file: Fingerprint,
    info: Info,
}

fn duration(path: &Path) -> Option<f64> {
    let mut reader = Reader::open(path).ok()?;
    match &mut reader.source {
        Source::Standard { format, track, .. } => {
            let params = &format.tracks().iter().find(|t| t.id == *track)?.codec_params;
            let frames = params.n_frames?;
            Some(params.time_base.map_or(frames as f64 / reader.rate as f64, |tb| {
                let t = tb.calc_time(frames);
                t.seconds as f64 + t.frac
            }))
        }
        Source::Opus { packets, preskip, .. } => {
            // No PCM decoding is required to learn an Ogg Opus stream's length.
            while let Some(packet) = packets.read_packet().ok()? {
                if packet.last_in_stream() {
                    return Some(packet.absgp_page().saturating_sub(*preskip as u64) as f64 / RATE as f64);
                }
            }
            None
        }
    }
}

pub fn inspect_cached(path: &Path, cache: &Path, report_duration: impl Fn(f64)) -> Result<Info, String> {
    let meta = path.metadata().map_err(err)?;
    let file = Fingerprint {
        path: path.canonicalize().map_err(err)?,
        bytes: meta.len(),
        modified: meta
            .modified()
            .map_err(err)?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(err)?
            .as_nanos(),
    };
    let mut key = std::collections::hash_map::DefaultHasher::new();
    file.path.hash(&mut key);
    let saved = cache.join(format!("audio-{:016x}.json", key.finish()));
    if let Ok(data) = std::fs::read(&saved)
        && let Ok(cached) = serde_json::from_slice::<Cached>(&data)
        && cached.file == file
        && cached.info.seconds.is_finite()
        && cached.info.seconds > 0.0
        && cached.info.seconds <= 6.0 * 3600.0
        && !cached.info.peaks.is_empty()
        && cached.info.peaks.len() <= 1024
        && cached.info.peaks.iter().all(|v| *v <= 7)
    {
        return Ok(cached.info);
    }
    if let Some(seconds) = duration(path).filter(|s| s.is_finite() && *s > 0.0 && *s <= 6.0 * 3600.0) {
        report_duration(seconds);
    }
    let info = inspect(path)?;
    // Optional cache: a read-only/full cache must not prevent playback or analysis.
    if std::fs::create_dir_all(cache).is_ok()
        && let Ok(data) = serde_json::to_vec(&Cached {
            file,
            info: info.clone(),
        })
    {
        let temporary = saved.with_extension(format!("json.{}.tmp", std::process::id()));
        if std::fs::write(&temporary, data).is_ok() {
            let _ = std::fs::rename(&temporary, &saved);
            let _ = std::fs::remove_file(temporary);
        }
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(path: &Path, seconds: u32) {
        let bytes = seconds * 8000 * 2;
        let mut data = b"RIFF".to_vec();
        data.extend((36 + bytes).to_le_bytes());
        data.extend(b"WAVEfmt ");
        data.extend(16u32.to_le_bytes());
        data.extend(1u16.to_le_bytes());
        data.extend(1u16.to_le_bytes());
        data.extend(8000u32.to_le_bytes());
        data.extend(16000u32.to_le_bytes());
        data.extend(2u16.to_le_bytes());
        data.extend(16u16.to_le_bytes());
        data.extend(b"data");
        data.extend(bytes.to_le_bytes());
        for n in 0..seconds * 8000 {
            let sample = if n < 8000 { 0 } else { 4096i16 };
            data.extend(sample.to_le_bytes());
        }
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn long_pcm_seek_starts_at_the_target_without_decoding_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.wav");
        wav(&path, 180);
        let mut reader = Reader::open(&path).unwrap();
        let skip = reader.seek(150.0).unwrap();
        assert!(
            skip < 8000,
            "demuxer must seek near the target, not skip 150 seconds of decoded audio"
        );
        let pcm = reader.next().unwrap().unwrap();
        assert!(pcm.iter().any(|s| *s > 0.1));
        assert_eq!(duration(&path), Some(180.0));
    }

    #[test]
    fn opus_seek_decodes_only_preroll_and_preserves_end_trim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.ogg");
        let mut writer = VoiceWriter::new(&path).unwrap();
        let tone: Vec<_> = (0..RATE)
            .map(|n| (n as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.4)
            .collect();
        for _ in 0..90 {
            writer.push(&tone).unwrap();
        }
        writer.finish().unwrap();
        drop(writer);
        assert_eq!(duration(&path), Some(90.0));
        let mut reader = Reader::open(&path).unwrap();
        assert_eq!(reader.seek(89.0).unwrap(), 0);
        if let Source::Opus { decoded, pending, .. } = &reader.source {
            assert!(*decoded > 88 * RATE as u64);
            assert!(pending.len() <= 6, "only decoder pre-roll is retained");
        }
        let mut frames = 0;
        let mut calls = 0;
        while let Some(pcm) = reader.next().unwrap() {
            frames += pcm.len() / reader.channels;
            calls += 1;
        }
        assert_eq!(
            frames, RATE as usize,
            "seek and final granule must keep exactly one second"
        );
        assert!(calls < 60, "the first 89 seconds must not be decoded");
    }

    #[test]
    fn waveform_cache_reports_duration_early_and_invalidates_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice.wav");
        let cache = dir.path().join("analysis");
        wav(&path, 2);
        let duration = std::cell::Cell::new(0.0);
        let first = inspect_cached(&path, &cache, |s| duration.set(s)).unwrap();
        assert_eq!(duration.get(), 2.0);
        assert_eq!(first.seconds, 2.0);
        assert_eq!(
            inspect_cached(&path, &cache, |_| panic!("cached waveform must not be rescanned")).unwrap(),
            first
        );
        wav(&path, 3);
        assert_eq!(
            inspect_cached(&path, &cache, |s| duration.set(s))
                .unwrap()
                .seconds,
            3.0
        );
        assert_eq!(duration.get(), 3.0);
    }
}
