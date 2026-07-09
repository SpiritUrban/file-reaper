//! T-071: витяг ключового кадру відео + тривалість (ffmpeg-функціональність).
//!
//! Реалізація порту [`VideoFrameSource`] через **ffmpeg як sidecar-процес**
//! (`std::process`, без нативного лінкування):
//!
//! - **мультиплатформність** — ffmpeg однаковий на Windows/macOS/Linux,
//!   адаптер не прив'язаний до ОС (на відміну від T-069/T-070, які є
//!   Windows-ланками того самого ланцюжка);
//! - **ізоляція збоїв задарма** (задел під T-074) — краш декодера на битому
//!   файлі вбиває *дочірній процес*, не застосунок;
//! - **покриття кодеків** — усі поширені контейнери/кодеки збірки ffmpeg.
//!
//! Пошук бінарника: змінна `TRASHRADAR_FFMPEG` (явний шлях) → `PATH`.
//! Немає ffmpeg → джерело прозоро віддає `Ok(None)` (ланцюжок далі),
//! відео отримують типізовану плитку без превью — деградація, не збій.

use std::path::PathBuf;
use std::process::Command;

use trashradar_app::ports::{RawThumbnail, ScrubStrip, VideoFrameSource, VideoMetadata};
use trashradar_domain::error::CoreError;

use crate::image::fit_within;

/// Змінна середовища з явним шляхом до бінарника ffmpeg.
pub const FFMPEG_ENV_VAR: &str = "TRASHRADAR_FFMPEG";

/// Джерело кадрів відео на базі sidecar-процесу ffmpeg (T-071).
#[derive(Debug, Clone)]
pub struct FfmpegVideoFrameSource {
    ffmpeg: Option<PathBuf>,
}

impl FfmpegVideoFrameSource {
    /// Створити джерело з автопошуком бінарника (`TRASHRADAR_FFMPEG` → `PATH`).
    pub fn new() -> Self {
        Self {
            ffmpeg: discover_ffmpeg(),
        }
    }

    /// Створити джерело з явним шляхом до ffmpeg (тести, майбутній конфіг).
    pub fn with_binary(path: PathBuf) -> Self {
        Self { ffmpeg: Some(path) }
    }

    /// Чи знайдено бінарник ffmpeg (для health-діагностики).
    pub fn is_available(&self) -> bool {
        self.ffmpeg.is_some()
    }

    /// Запустити ffmpeg з аргументами; `None` — процес не вдалося виконати.
    fn run(&self, args: &[&str]) -> Option<std::process::Output> {
        let ffmpeg = self.ffmpeg.as_ref()?;
        let mut cmd = Command::new(ffmpeg);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Без миготіння консольного вікна поруч з GUI-застосунком.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.output().ok()
    }
}

impl Default for FfmpegVideoFrameSource {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoFrameSource for FfmpegVideoFrameSource {
    fn probe(&self, path: &str) -> Result<Option<VideoMetadata>, CoreError> {
        if path.is_empty() {
            return Err(CoreError::invalid_argument("Порожній шлях до відеофайла."));
        }
        // `ffmpeg -i` без вихідного файла завершується з помилкою, але
        // друкує метадані контейнера у stderr — цього достатньо для probe.
        let Some(output) = self.run(&["-hide_banner", "-i", path]) else {
            return Ok(None);
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(parse_video_metadata(&stderr))
    }

    fn key_frame(&self, path: &str, max_edge: u32) -> Result<Option<RawThumbnail>, CoreError> {
        if path.is_empty() {
            return Err(CoreError::invalid_argument("Порожній шлях до відеофайла."));
        }
        let edge = max_edge.max(1);
        let Some(meta) = self.probe(path)? else {
            return Ok(None);
        };
        let (out_w, out_h) = fit_within(meta.width, meta.height, edge);

        // Кадр з ~10% тривалості (повз чорні вступні кадри); `-ss` перед
        // `-i` — швидкий seek до найближчого ключового кадру. Для дуже
        // коротких/бідних на кадри файлів — фолбек з початку файла.
        let primary_seek = meta.duration_ms / 10;
        for seek_ms in [primary_seek, 0] {
            if let Some(frame) = self.extract_frame(path, seek_ms, out_w, out_h) {
                return Ok(Some(frame));
            }
            if primary_seek == 0 {
                break; // обидві спроби ідентичні
            }
        }
        Ok(None)
    }

    fn scrub_strip(
        &self,
        path: &str,
        max_edge: u32,
        frames: u32,
    ) -> Result<Option<ScrubStrip>, CoreError> {
        if path.is_empty() {
            return Err(CoreError::invalid_argument("Порожній шлях до відеофайла."));
        }
        let edge = max_edge.max(1);
        let frames = clamp_scrub_frames(frames);
        let Some(meta) = self.probe(path)? else {
            return Ok(None);
        };
        let (out_w, out_h) = fit_within(meta.width, meta.height, edge);

        // Один прохід декодування (DoD T-072): один запуск ffmpeg без seek —
        // фільтр `fps=frames/duration` семплить кадри рівномірно по таймлайну
        // (кадр i ≈ момент duration·i/frames), `-frames:v` — стеля кількості.
        let filter = build_scrub_filter(frames, meta.duration_ms, out_w, out_h);
        let frames_arg = frames.to_string();
        let output = self.run(&[
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            path,
            "-an",
            "-sn",
            "-frames:v",
            &frames_arg,
            "-vf",
            &filter,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgra",
            "pipe:1",
        ]);
        let Some(output) = output else {
            return Ok(None);
        };

        let frame_bytes = (out_w as usize) * (out_h as usize) * 4;
        let len = output.stdout.len();
        // Строгий контракт буфера: лише цілі кадри, хоча б один.
        if !output.status.success() || len == 0 || len % frame_bytes != 0 {
            return Ok(None);
        }
        Ok(Some(ScrubStrip {
            frame_width: out_w,
            frame_height: out_h,
            frame_count: (len / frame_bytes) as u32,
            bgra: output.stdout,
        }))
    }
}

impl FfmpegVideoFrameSource {
    /// Витягти один кадр `out_w×out_h` BGRA з позиції `seek_ms`.
    fn extract_frame(
        &self,
        path: &str,
        seek_ms: u64,
        out_w: u32,
        out_h: u32,
    ) -> Option<RawThumbnail> {
        let seek = format_seek(seek_ms);
        let scale = format!("scale={out_w}:{out_h}");
        let output = self.run(&[
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &seek,
            "-i",
            path,
            "-an",
            "-sn",
            "-frames:v",
            "1",
            "-vf",
            &scale,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgra",
            "pipe:1",
        ])?;

        let expected = (out_w as usize) * (out_h as usize) * 4;
        if !output.status.success() || output.stdout.len() != expected {
            return None;
        }
        Some(RawThumbnail {
            width: out_w,
            height: out_h,
            bgra: output.stdout,
        })
    }
}

/// Знайти бінарник ffmpeg: `TRASHRADAR_FFMPEG` (явний шлях) → пошук у `PATH`.
fn discover_ffmpeg() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(FFMPEG_ENV_VAR) {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe_name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Розібрати метадані відео зі stderr `ffmpeg -i`: тривалість + перший
/// відеопотік. Аудіофайли (без рядка `Video:`) і потоки без тривалості
/// (`Duration: N/A`) — не відео для превью → `None`.
fn parse_video_metadata(stderr: &str) -> Option<VideoMetadata> {
    let duration_ms = parse_duration_ms(stderr)?;
    let (width, height) = parse_video_dimensions(stderr)?;
    Some(VideoMetadata {
        duration_ms,
        width,
        height,
    })
}

/// Знайти `Duration: HH:MM:SS.cc` і перевести в мілісекунди.
fn parse_duration_ms(stderr: &str) -> Option<u64> {
    let idx = stderr.find("Duration: ")?;
    let rest = &stderr[idx + "Duration: ".len()..];
    let token = rest.split([',', '\r', '\n']).next()?.trim();
    parse_timestamp_ms(token)
}

/// `HH:MM:SS.cc` → мілісекунди (`N/A` та сміття → `None`).
fn parse_timestamp_ms(token: &str) -> Option<u64> {
    let mut parts = token.split(':');
    let hours: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next()?.parse().ok()?;
    let seconds_part = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let (seconds, centis): (u64, u64) = match seconds_part.split_once('.') {
        Some((s, c)) => {
            let fraction: u64 = c.parse().ok()?;
            // ffmpeg друкує соті секунди (2 знаки).
            let scale = 10u64.pow(3u32.saturating_sub(c.len() as u32));
            (s.parse().ok()?, fraction * scale)
        }
        None => (seconds_part.parse().ok()?, 0),
    };
    Some(((hours * 60 + minutes) * 60 + seconds) * 1000 + centis)
}

/// Знайти розмір кадру `WxH` у першому рядку `Stream … Video: …`.
fn parse_video_dimensions(stderr: &str) -> Option<(u32, u32)> {
    let video_line = stderr
        .lines()
        .find(|line| line.contains("Stream #") && line.contains("Video:"))?;
    // Розмір — сегмент вигляду `320x240` (можливо з хвостом ` [SAR …]`),
    // відокремлений комами від кодека/пікформату.
    for segment in video_line.split(", ") {
        let head = segment.split_whitespace().next().unwrap_or("");
        if let Some((w, h)) = head.split_once('x') {
            if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                if w > 0 && h > 0 {
                    return Some((w, h));
                }
            }
        }
    }
    None
}

/// Позиція seek для ffmpeg: мілісекунди → `SS.mmm`.
fn format_seek(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Межі кількості кадрів скраб-смуги (architecture.md §5.2: 10–20;
/// нижня межа 2 — щоб «смуга» завжди мала рух таймлайну).
fn clamp_scrub_frames(frames: u32) -> u32 {
    frames.clamp(2, 20)
}

/// Фільтр одного проходу для смуги: рівномірний семплінг `frames` кадрів
/// по всій тривалості + даунскейл. Частота — цілим дробом `frames*1000 / ms`
/// (без плаваючої коми в аргументах ffmpeg).
fn build_scrub_filter(frames: u32, duration_ms: u64, out_w: u32, out_h: u32) -> String {
    let num = (frames as u64) * 1000;
    let den = duration_ms.max(1);
    format!("fps={num}/{den},scale={out_w}:{out_h}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_STDERR: &str = "\
Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'clip.mp4':
  Metadata:
    major_brand     : isom
  Duration: 00:01:23.45, start: 0.000000, bitrate: 1205 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 1920x1080 [SAR 1:1 DAR 16:9], 1128 kb/s, 30 fps, 30 tbr, 15360 tbn (default)
  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 69 kb/s (default)
";

    #[test]
    fn parses_duration_to_milliseconds() {
        assert_eq!(parse_duration_ms(SAMPLE_STDERR), Some(83_450));
    }

    #[test]
    fn parses_video_dimensions_from_stream_line() {
        assert_eq!(parse_video_dimensions(SAMPLE_STDERR), Some((1920, 1080)));
    }

    #[test]
    fn parses_full_metadata() {
        let meta = parse_video_metadata(SAMPLE_STDERR).expect("metadata");
        assert_eq!(meta.duration_ms, 83_450);
        assert_eq!((meta.width, meta.height), (1920, 1080));
    }

    #[test]
    fn audio_only_file_is_not_video() {
        let stderr = "\
Input #0, mp3, from 'song.mp3':
  Duration: 00:03:00.00, start: 0.000000, bitrate: 320 kb/s
  Stream #0:0: Audio: mp3, 44100 Hz, stereo, fltp, 320 kb/s
";
        assert_eq!(parse_video_metadata(stderr), None);
    }

    #[test]
    fn missing_duration_is_none() {
        let stderr = "Input #0\n  Duration: N/A, bitrate: N/A\n  Stream #0:0: Video: h264, yuv420p, 640x480\n";
        assert_eq!(parse_video_metadata(stderr), None);
    }

    #[test]
    fn timestamp_parsing_handles_hours_and_fraction() {
        assert_eq!(parse_timestamp_ms("01:02:03.04"), Some(3_723_040));
        assert_eq!(parse_timestamp_ms("00:00:05.00"), Some(5_000));
        assert_eq!(parse_timestamp_ms("garbage"), None);
        assert_eq!(parse_timestamp_ms("N/A"), None);
    }

    #[test]
    fn seek_formatting_is_seconds_dot_millis() {
        assert_eq!(format_seek(0), "0.000");
        assert_eq!(format_seek(8_345), "8.345");
    }

    #[test]
    fn empty_path_is_invalid_argument() {
        let src = FfmpegVideoFrameSource::with_binary(PathBuf::from("ffmpeg"));
        let err = src.probe("").expect_err("порожній шлях = помилка");
        assert_eq!(
            err.code,
            trashradar_domain::error::ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn missing_binary_is_transparent_none() {
        // Бінарник відсутній → джерело не покриває файл (деградація, не збій).
        let src = FfmpegVideoFrameSource::with_binary(PathBuf::from(
            r"C:\definitely\missing\ffmpeg_ktr71.exe",
        ));
        assert!(src.probe("whatever.mp4").expect("no error").is_none());
        assert!(src
            .key_frame("whatever.mp4", 96)
            .expect("no error")
            .is_none());
        assert!(src
            .scrub_strip("whatever.mp4", 96, 12)
            .expect("no error")
            .is_none());
    }

    #[test]
    fn scrub_frames_clamped_to_spec_range() {
        assert_eq!(clamp_scrub_frames(0), 2);
        assert_eq!(clamp_scrub_frames(16), 16);
        assert_eq!(clamp_scrub_frames(100), 20);
    }

    #[test]
    fn scrub_filter_samples_uniformly_without_floats() {
        assert_eq!(
            build_scrub_filter(12, 4_000, 96, 72),
            "fps=12000/4000,scale=96:72"
        );
        // Нульова тривалість не дає ділення на нуль.
        assert_eq!(build_scrub_filter(16, 0, 64, 36), "fps=16000/1,scale=64:36");
    }

    #[test]
    fn scrub_strip_frame_slicing() {
        let strip = ScrubStrip {
            frame_width: 2,
            frame_height: 1,
            frame_count: 3,
            bgra: (0..24).collect(),
        };
        assert_eq!(strip.frame_bytes(), 8);
        assert_eq!(strip.frame_slice(0).unwrap(), &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(strip.frame_slice(2).unwrap()[0], 16);
        assert!(strip.frame_slice(3).is_none());
    }

    /// DoD T-071 (потребує реального ffmpeg: `TRASHRADAR_FFMPEG` або PATH):
    /// поширені контейнери/кодеки дають кадр; тривалість визначається.
    /// Тестові відео генеруються самим ffmpeg (lavfi testsrc) — без
    /// закомічених бінарних файлів у репозиторії.
    #[test]
    #[ignore = "DoD: потребує бінарника ffmpeg (TRASHRADAR_FFMPEG або PATH)"]
    fn real_videos_yield_frame_and_duration() {
        let src = FfmpegVideoFrameSource::new();
        assert!(
            src.is_available(),
            "встановіть ffmpeg або вкажіть TRASHRADAR_FFMPEG"
        );

        let dir = std::env::temp_dir().join("tr_t071_video");
        std::fs::create_dir_all(&dir).unwrap();

        // Поширені контейнери/кодеки: mp4/h264, mkv/h264, avi/mpeg4, mov/h264.
        let variants: [(&str, &[&str]); 4] = [
            ("clip.mp4", &["-c:v", "libx264"]),
            ("clip.mkv", &["-c:v", "libx264"]),
            ("clip.avi", &["-c:v", "mpeg4"]),
            ("clip.mov", &["-c:v", "libx264"]),
        ];

        for (name, codec_args) in variants {
            let out = dir.join(name);
            let mut args: Vec<&str> = vec![
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=4:size=320x240:rate=10",
                "-pix_fmt",
                "yuv420p",
            ];
            args.extend_from_slice(codec_args);
            let out_str = out.to_string_lossy().to_string();
            args.push(&out_str);
            let status = src.run(&args).expect("ffmpeg run");
            assert!(status.status.success(), "{name}: генерація не вдалася");

            // Тривалість визначається (DoD): 4 с ± контейнерні похибки.
            let meta = src
                .probe(&out_str)
                .expect("probe call")
                .unwrap_or_else(|| panic!("{name}: probe мав розпізнати відео"));
            assert!(
                (3_500..=4_500).contains(&meta.duration_ms),
                "{name}: тривалість {} мс замість ~4000",
                meta.duration_ms
            );
            assert_eq!((meta.width, meta.height), (320, 240), "{name}: розмір");

            // Кадр дається (DoD): вписаний у 96px, точний буфер BGRA.
            let frame = src
                .key_frame(&out_str, 96)
                .expect("key_frame call")
                .unwrap_or_else(|| panic!("{name}: очікували кадр"));
            assert_eq!((frame.width, frame.height), (96, 72), "{name}: даунскейл");
            assert_eq!(frame.bgra.len(), 96 * 72 * 4, "{name}: буфер");
            // testsrc — кольорова таблиця: кадр не має бути суцільно чорним.
            assert!(
                frame
                    .bgra
                    .chunks(4)
                    .any(|px| px[0] > 8 || px[1] > 8 || px[2] > 8),
                "{name}: кадр підозріло порожній"
            );
        }

        // Не-відео (текст) → None, не помилка.
        let bogus = dir.join("not_video.txt");
        std::fs::write(&bogus, b"no frames here").unwrap();
        assert!(src
            .probe(&bogus.to_string_lossy())
            .expect("probe call")
            .is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DoD T-072 (потребує реального ffmpeg): смуга 10–20 кадрів по
    /// таймлайну генерується **одним** запуском ffmpeg (один прохід
    /// декодування, без per-frame seek); кадри покривають таймлайн.
    #[test]
    #[ignore = "DoD: потребує бінарника ffmpeg (TRASHRADAR_FFMPEG або PATH)"]
    fn real_video_scrub_strip_in_single_pass() {
        let src = FfmpegVideoFrameSource::new();
        assert!(
            src.is_available(),
            "встановіть ffmpeg або вкажіть TRASHRADAR_FFMPEG"
        );

        let dir = std::env::temp_dir().join("tr_t072_scrub");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("clip.mp4");
        let out_str = out.to_string_lossy().to_string();
        let status = src
            .run(&[
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=4:size=320x240:rate=10",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
                &out_str,
            ])
            .expect("ffmpeg run");
        assert!(status.status.success(), "генерація не вдалася");

        let strip = src
            .scrub_strip(&out_str, 96, 12)
            .expect("scrub call")
            .expect("очікували смугу кадрів");

        // Геометрія: 12 кадрів 96×72 (даунскейл 320×240 → fit 96).
        assert_eq!((strip.frame_width, strip.frame_height), (96, 72));
        assert_eq!(strip.frame_count, 12, "12 кадрів у межах 10–20 за §5.2");
        assert_eq!(
            strip.bgra.len(),
            strip.frame_bytes() * strip.frame_count as usize,
            "буфер — рівно цілі кадри"
        );

        // Кадри покривають таймлайн, а не один задубльований кадр:
        // testsrc має рухомий лічильник — перший і останній кадри різняться.
        let first = strip.frame_slice(0).unwrap();
        let last = strip.frame_slice(strip.frame_count - 1).unwrap();
        assert_ne!(first, last, "кадри мають відрізнятися вздовж таймлайну");

        // Запит поза межами → None, без панік.
        assert!(strip.frame_slice(strip.frame_count).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
