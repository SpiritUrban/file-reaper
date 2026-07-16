//! T-155: стрес-тест конвеєра превью на корпусі битих/екзотичних медіафайлів.
//!
//! **DoD:** 0 крашів застосунку на корпусі; всі збої деградують у «превью
//! недоступне». Тест ганяє **реальний** ланцюжок джерел (T-069 системні
//! мініатюри → T-070 декодування WIC → T-071 кадр відео через ffmpeg) по
//! кожному файлу корпусу з `testkit` (T-155) і перевіряє два інваріанти:
//!
//! 1. **Процес живий.** Краш декодера (панік, порушення пам'яті, зависання
//!    дочірнього ffmpeg) завалив би сам тестовий бінарник — тобто DoD «0 крашів»
//!    перевіряється машиною в звичайному `cargo test`, а не оком.
//! 2. **Деградація, а не збій.** Для кожного файла результат — або валідна
//!    мініатюра (геометрія і буфер узгоджені, розмір обмежений), або «превью
//!    недоступне» (`None`). Помилка джерела чи побитий буфер — провал тесту.
//!
//! Ізоляція збоїв, яку тест вправляє: панік-бар'єри T-074 у `generate_from_chain`
//! та у воркерах планувальника (T-067) + окремий процес ffmpeg (T-071).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use trashradar_app::ports::{RawThumbnail, ThumbnailSource, VideoFrameSource};
use trashradar_app::preview::{
    encode_thumbnail, generate_from_chain, PreviewPriority, PreviewScheduler, VideoKeyFrameSource,
};
use trashradar_testkit::media_corpus::MediaCorpus;

use crate::{
    FfmpegVideoFrameSource, ImageThumbnailSource, PngThumbnailEncoder, WindowsShellThumbnailSource,
};

/// Край мініатюри плитки (P1-шлях конвеєра).
const MAX_EDGE: u32 = 128;

/// Реальний ланцюжок джерел превью — той самий склад і порядок, що збирає
/// IPC-міст у `core/shell` (T-120): системний кеш → WIC → кадр відео.
fn real_chain() -> Vec<Arc<dyn ThumbnailSource>> {
    vec![
        Arc::new(WindowsShellThumbnailSource::new()),
        Arc::new(ImageThumbnailSource::new()),
        Arc::new(VideoKeyFrameSource(Arc::new(FfmpegVideoFrameSource::new()))),
    ]
}

/// Мініатюра, віддана з битого файла, мусить бути придатною до показу:
/// непорожня, вписана в `max_edge`, буфер узгоджений з геометрією.
fn assert_usable_thumbnail(thumb: &RawThumbnail, max_edge: u32, label: &str) {
    assert!(
        thumb.width > 0 && thumb.height > 0,
        "«{label}»: віддано порожню мініатюру {}x{}",
        thumb.width,
        thumb.height
    );
    assert!(
        thumb.width <= max_edge && thumb.height <= max_edge,
        "«{label}»: мініатюра {}x{} не вписана в {max_edge}",
        thumb.width,
        thumb.height
    );
    assert_eq!(
        thumb.bgra.len(),
        (thumb.width as usize) * (thumb.height as usize) * 4,
        "«{label}»: буфер не відповідає геометрії {}x{}",
        thumb.width,
        thumb.height
    );
}

/// DoD T-155: жоден файл корпусу не валить процес; кожен збій — це «превью
/// недоступне», а не помилка, паніка чи побитий буфер.
#[test]
fn broken_media_corpus_degrades_to_preview_unavailable_without_crashing() {
    let corpus = MediaCorpus::generate_temp("t155_chain").expect("згенерувати корпус");
    let chain = real_chain();
    let encoder = PngThumbnailEncoder::new();

    let mut unavailable = 0usize;
    let mut decoded: Vec<&str> = Vec::new();
    for file in corpus.files() {
        match generate_from_chain(&chain, &file.path_str(), MAX_EDGE) {
            // «Превью недоступне»: UI покаже типізовану плитку (ui.md §5).
            None => unavailable += 1,
            // Деякі биті файли декодер усе ж витягує частково — це дозволено,
            // але тоді результат мусить бути придатним до показу і кодування.
            Some(thumb) => {
                assert_usable_thumbnail(&thumb, MAX_EDGE, file.label);
                assert!(
                    encode_thumbnail(&encoder, &thumb).is_some(),
                    "«{}»: кодувальник кешу не впорався з відданою мініатюрою",
                    file.label
                );
                decoded.push(file.label);
            }
        }
    }

    // Розклад корпусу — у логу тесту: які саме биті файли декодер усе ж витяг
    // (склад залежить від набору кодеків машини і сам по собі не є вимогою).
    println!(
        "T-155: корпус {} файлів → {} з превью {:?}, {unavailable} «превью недоступне»",
        corpus.files().len(),
        decoded.len(),
        decoded
    );
    assert_eq!(
        decoded.len() + unavailable,
        corpus.files().len(),
        "кожен файл корпусу мусить мати визначений результат"
    );
    assert!(
        unavailable > 0,
        "корпус не побитий: жоден файл не деградував у «превью недоступне»"
    );
}

/// Контракт ланки ланцюжка (architecture.md §5.2): битий файл — це «джерело
/// його не покриває» (`Ok(None)`), а не `Err`. Інакше збій одного декодера
/// зривав би весь конвеєр превью.
#[test]
fn every_source_reports_broken_media_as_not_covered_not_error() {
    let corpus = MediaCorpus::generate_temp("t155_sources").expect("згенерувати корпус");
    let sources = real_chain();

    for file in corpus.files() {
        let path = file.path_str();
        for (index, source) in sources.iter().enumerate() {
            let outcome = source.thumbnail(&path, MAX_EDGE).unwrap_or_else(|err| {
                panic!(
                    "джерело #{index} на «{}» повернуло помилку замість деградації: {err:?}",
                    file.label
                )
            });
            if let Some(thumb) = outcome {
                assert_usable_thumbnail(&thumb, MAX_EDGE, file.label);
            }
        }
    }
}

/// Побиті контейнери відео: `probe`/`key_frame`/`scrub_strip` деградують у
/// `None`, а не в помилку чи побиту смугу кадрів. Без ffmpeg у системі джерело
/// прозоро віддає `None` (T-071) — тест лишається валідним.
#[test]
fn video_source_degrades_on_broken_containers() {
    let corpus = MediaCorpus::generate_temp("t155_video").expect("згенерувати корпус");
    let video = FfmpegVideoFrameSource::new();
    let edge = 96;

    for file in corpus.files() {
        let path = file.path_str();

        let probed = video
            .probe(&path)
            .unwrap_or_else(|err| panic!("probe «{}» → помилка: {err:?}", file.label));
        if let Some(meta) = probed {
            // Метадані з битого контейнера можуть бути будь-якими, але
            // не можуть бути нулями — на них рахується геометрія кадру.
            assert!(
                meta.width > 0 && meta.height > 0,
                "«{}»: probe віддав нульову геометрію {}x{}",
                file.label,
                meta.width,
                meta.height
            );
        }

        let frame = video
            .key_frame(&path, edge)
            .unwrap_or_else(|err| panic!("key_frame «{}» → помилка: {err:?}", file.label));
        if let Some(frame) = frame {
            assert_usable_thumbnail(&frame, edge, file.label);
        }

        let strip = video
            .scrub_strip(&path, edge, 12)
            .unwrap_or_else(|err| panic!("scrub_strip «{}» → помилка: {err:?}", file.label));
        if let Some(strip) = strip {
            assert!(
                strip.frame_count > 0,
                "«{}»: смуга без кадрів замість None",
                file.label
            );
            assert_eq!(
                strip.bgra.len(),
                strip.frame_bytes() * strip.frame_count as usize,
                "«{}»: буфер смуги не кратний розміру кадру",
                file.label
            );
        }
    }
}

/// Ізоляція воркерів на реальному корпусі (T-074 у бойових умовах): весь корпус
/// проганяється через планувальник паралельно; жодна задача не «губиться», а
/// пул переживає корпус і виконує наступну задачу.
#[test]
fn preview_worker_pool_survives_whole_corpus_under_load() {
    let corpus = MediaCorpus::generate_temp("t155_pool").expect("згенерувати корпус");
    let chain = real_chain();
    let scheduler = PreviewScheduler::new(4, 0.0);
    let completed = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel();

    for file in corpus.files() {
        let path = file.path_str();
        let chain = chain.clone();
        let completed = Arc::clone(&completed);
        let tx = tx.clone();
        scheduler.submit(path.clone(), PreviewPriority::P1, move |_| {
            let _ = generate_from_chain(&chain, &path, MAX_EDGE);
            completed.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(());
        });
    }
    drop(tx);

    for _ in 0..corpus.files().len() {
        rx.recv_timeout(Duration::from_secs(60))
            .expect("задача превью не завершилась: воркер помер або зависнув на битому файлі");
    }
    assert_eq!(completed.load(Ordering::SeqCst), corpus.files().len());

    // Канарка: після всього корпусу пул і далі приймає й виконує задачі.
    let (canary_tx, canary_rx) = mpsc::channel();
    scheduler.submit("canary".to_string(), PreviewPriority::P0, move |_| {
        let _ = canary_tx.send(());
    });
    canary_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("пул воркерів мав пережити корпус битих файлів");
}

/// Екзотика, яка НЕ має деградувати: користувач перейменував зображення у
/// `.mp4`. Декодер працює за вмістом, а не за розширенням (T-070) — превью є.
/// Заразом це контрольний випадок для корпусу: `None` вище — не тому, що
/// ланцюжок мовчить на всьому підряд.
#[cfg(windows)]
#[test]
fn valid_image_under_video_extension_is_decoded_by_content() {
    let dir = std::env::temp_dir().join("tr_t155_mismatch");
    std::fs::create_dir_all(&dir).expect("тимчасовий каталог");
    let path = dir.join("actually_a_png.mp4");
    std::fs::write(&path, crate::test_media::make_png(96, 48, [255, 0, 0])).expect("записати файл");

    let thumb = generate_from_chain(&real_chain(), &path.to_string_lossy(), MAX_EDGE)
        .expect("вміст PNG має декодуватися попри розширення .mp4");
    assert_usable_thumbnail(&thumb, MAX_EDGE, "PNG під розширенням .mp4");

    let _ = std::fs::remove_dir_all(&dir);
}
