//! Кандидат на видалення — центральна сутність продукту.

use serde::{Deserialize, Serialize};

use crate::category::CategoryId;

/// Стабільний ідентифікатор запису в індексі.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateId(pub u64);

/// Розмір у байтах (value object, щоб не плутати з іншими u64).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ByteSize(pub u64);

/// Часова мітка FS (Windows FILETIME, 100-нс інтервали від 1601-01-01 UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FsTimestamp(pub i64);

/// Клас файла за розширенням (T-023).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Video,
    Image,
    Audio,
    Archive,
    Installer,
    DiskImage,
    Document,
    Other,
}

impl FileKind {
    /// Класифікує файл за розширенням у шляху чи імені (T-023).
    /// Файли без розширення та dotfiles (`.gitignore`) — [`FileKind::Other`].
    pub fn from_path(path: &str) -> FileKind {
        match extension_of(path) {
            Some(ext) => FileKind::from_extension(ext),
            None => FileKind::Other,
        }
    }

    /// Класифікує за самим розширенням (без крапки, регістр не має значення).
    /// Невідоме розширення → [`FileKind::Other`] (docs/tasks.md T-023).
    ///
    /// Код і конфіги свідомо лишаються `Other`: dev-артефакти визначають
    /// структурні детектори (T-049+), не розширення.
    pub fn from_extension(ext: &str) -> FileKind {
        // Розширення короткі й ASCII — порівнюємо без регістру без алокацій.
        let lower = ext.to_ascii_lowercase();
        match lower.as_str() {
            "mp4" | "m4v" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "mpg" | "mpeg"
            | "m2ts" | "mts" | "ts" | "vob" | "3gp" | "3g2" | "ogv" | "rm" | "rmvb" | "divx"
            | "f4v" | "mxf" | "asf" | "braw" | "r3d" => FileKind::Video,

            "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "gif" | "bmp" | "dib" | "tif" | "tiff"
            | "webp" | "heic" | "heif" | "avif" | "ico" | "svg" | "wmf" | "emf" | "psd" | "ai"
            | "eps" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "orf" | "rw2" | "raf" | "sr2"
            | "pef" => FileKind::Image,

            "mp3" | "wav" | "flac" | "aac" | "ogg" | "oga" | "m4a" | "wma" | "aiff" | "aif"
            | "alac" | "opus" | "mid" | "midi" | "ape" | "wv" => FileKind::Audio,

            "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "tbz2" | "xz" | "txz" | "zst"
            | "cab" | "arj" | "lzh" | "lha" | "z" | "lz4" => FileKind::Archive,

            "exe" | "msi" | "msix" | "appx" | "appxbundle" | "msu" | "apk" | "aab" | "deb"
            | "rpm" | "dmg" | "pkg" => FileKind::Installer,

            "iso" | "img" | "vhd" | "vhdx" | "vmdk" | "nrg" | "mdf" | "cue" => FileKind::DiskImage,

            "pdf" | "doc" | "docx" | "dot" | "dotx" | "xls" | "xlsx" | "xlsm" | "ppt" | "pptx"
            | "txt" | "rtf" | "odt" | "ods" | "odp" | "csv" | "tsv" | "md" | "markdown"
            | "epub" | "mobi" | "azw3" | "djvu" => FileKind::Document,

            _ => FileKind::Other,
        }
    }
}

/// Витягує розширення (без крапки) з імені файла в кінці шляху.
/// `None` для файлів без розширення та dotfiles (`.gitignore`).
fn extension_of(path: &str) -> Option<&str> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let dot = name.rfind('.')?;
    if dot == 0 {
        return None; // dotfile без розширення
    }
    let ext = &name[dot + 1..];
    if ext.is_empty() {
        None
    } else {
        Some(ext)
    }
}

/// Що показує сітка: окремий файл або папка-одиниця (node_modules тощо, T-053).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateUnit {
    File,
    Folder,
}

/// Рішення користувача щодо кандидата.
///
/// Живе на **файлі** (шлях / candidate_id), не на окремій категорії:
/// Keep/Mark в одній категорії одразу діє у всіх (architecture.md §6.3, T-057).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Рішення ще не прийняте.
    Undecided,
    /// Залишити: ховається з кандидатів, рішення персистентне (T-057).
    Keep,
    /// Позначено до видалення (Reap Bar).
    Marked,
}

impl Decision {
    /// Keep — не показуємо в сітці / агрегатах жодної категорії.
    pub fn hides_from_candidates(self) -> bool {
        matches!(self, Decision::Keep)
    }

    /// У Reap Bar (позначено до reap).
    pub fn is_marked_for_reap(self) -> bool {
        matches!(self, Decision::Marked)
    }

    /// У «можна звільнити» (не Keep).
    pub fn is_reclaimable(self) -> bool {
        !self.hides_from_candidates()
    }
}

/// Вердикт детектора: чому файл є кандидатом (docs/architecture.md §6.2, T-036).
///
/// Обов'язкові три поля: категорія, пояснення (рядок UI), рівень безпечності
/// (`safe-to-bulk` вмикає [Позначити все]; `review-recommended` — маркер «?»).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub category: CategoryId,
    /// Людське пояснення: «відео 4.2 ГБ, останній доступ 8 міс тому».
    pub explanation: String,
    pub safety: SafetyLevel,
}

impl Verdict {
    /// Зібрати вердикт. `explanation` має бути непорожнім (готовий до показу).
    pub fn new(category: CategoryId, explanation: impl Into<String>, safety: SafetyLevel) -> Self {
        let explanation = explanation.into();
        debug_assert!(
            !explanation.trim().is_empty(),
            "Verdict.explanation не може бути порожнім"
        );
        Self {
            category,
            explanation,
            safety,
        }
    }

    /// Чи вердикт валідний для показу в UI (усі три поля заповнені змістовно).
    pub fn is_complete(&self) -> bool {
        !self.explanation.trim().is_empty()
    }
}

/// Рівень безпечності масового видалення.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyLevel {
    /// Можна позначати категорію цілком ([Позначити все]).
    SafeToBulk,
    /// Рекомендований перегляд людиною (маркер «?» у REAP-флоу).
    ReviewRecommended,
}

/// Атрибути файлової системи (Windows-прапорці та додаткові біти).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAttributes {
    pub raw_bits: u32,
    pub is_readonly: bool,
    pub is_hidden: bool,
    pub is_system: bool,
    pub is_temporary: bool,
}

impl FileAttributes {
    /// Дегідратований хмарний плейсхолдер (OneDrive / Google DriveFS / інший
    /// HSM-провайдер): вміст файла НЕ локальний, будь-яке читання «гідратує»
    /// його — тобто тягне з хмари синхронною блокуючою мережевою операцією
    /// (а офлайн — блокує). Визначається за raw Windows-бітами (зберігаються
    /// в індексі через `raw_bits`): `OFFLINE`, `RECALL_ON_OPEN`,
    /// `RECALL_ON_DATA_ACCESS`. Повністю завантажений локально файл не має
    /// жодного з них.
    ///
    /// Каскад дублікатів (`app::duplicates`) пропускає такі файли: інакше
    /// stage 2 (head+tail 64 КіБ кожного файла size-колізії) завантажив би
    /// гігабайти хмарного вмісту й міг зависнути назавжди на першому ж
    /// офлайн-плейсхолдері.
    pub fn is_cloud_dehydrated(&self) -> bool {
        const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
        const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
        const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
        self.raw_bits
            & (FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_RECALL_ON_OPEN
                | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
            != 0
    }
}

/// Запис кандидата на видалення, збережений у persistent-індексі.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub candidate_id: CandidateId,
    pub path: String,
    pub size: ByteSize,
    pub created_at: Option<FsTimestamp>,
    pub modified_at: Option<FsTimestamp>,
    pub accessed_at: Option<FsTimestamp>,
    pub kind: FileKind,
    pub unit: CandidateUnit,
    pub category: CategoryId,
    pub safety: SafetyLevel,
    pub decision: Decision,
    pub detector_id: String,
    pub explanation: String,
    pub attributes: FileAttributes,
}

/// Варіанти сортування віконних запитів.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRecordSort {
    /// За розміром (від більших до менших).
    SizeDesc,
    /// За часом останнього доступу (від давніших до новіших).
    AccessedAsc,
    /// За часом останнього доступу (від новіших до давніших).
    AccessedDesc,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_dehydrated_detects_recall_and_offline_bits() {
        let cloud = |bits: u32| FileAttributes {
            raw_bits: bits,
            ..Default::default()
        };
        // OFFLINE / RECALL_ON_OPEN / RECALL_ON_DATA_ACCESS — кожен окремо.
        assert!(cloud(0x0000_1000).is_cloud_dehydrated());
        assert!(cloud(0x0004_0000).is_cloud_dehydrated());
        assert!(cloud(0x0040_0000).is_cloud_dehydrated());
        // Разом зі звичайними бітами (READONLY|ARCHIVE) — все одно так.
        assert!(cloud(0x0040_0000 | 0x0000_0021).is_cloud_dehydrated());
        // Повністю локальний файл (ARCHIVE|READONLY) — ні.
        assert!(!cloud(0x0000_0021).is_cloud_dehydrated());
        assert!(!FileAttributes::default().is_cloud_dehydrated());
    }

    #[test]
    fn classifies_representative_extensions_per_kind() {
        for ext in ["mp4", "mkv", "mov", "webm"] {
            assert_eq!(FileKind::from_extension(ext), FileKind::Video, "{ext}");
        }
        for ext in ["jpg", "png", "heic", "cr3", "psd", "wmf", "emf"] {
            assert_eq!(FileKind::from_extension(ext), FileKind::Image, "{ext}");
        }
        for ext in ["mp3", "flac", "wav", "opus"] {
            assert_eq!(FileKind::from_extension(ext), FileKind::Audio, "{ext}");
        }
        for ext in ["zip", "rar", "7z", "gz", "zst"] {
            assert_eq!(FileKind::from_extension(ext), FileKind::Archive, "{ext}");
        }
        for ext in ["exe", "msi", "msix", "apk"] {
            assert_eq!(FileKind::from_extension(ext), FileKind::Installer, "{ext}");
        }
        for ext in ["iso", "img", "vhdx", "vmdk"] {
            assert_eq!(FileKind::from_extension(ext), FileKind::DiskImage, "{ext}");
        }
        for ext in ["pdf", "docx", "xlsx", "txt", "epub"] {
            assert_eq!(FileKind::from_extension(ext), FileKind::Document, "{ext}");
        }
    }

    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(FileKind::from_extension("MP4"), FileKind::Video);
        assert_eq!(FileKind::from_extension("Jpg"), FileKind::Image);
        assert_eq!(FileKind::from_path("C:\\Movies\\CLIP.MKV"), FileKind::Video);
    }

    #[test]
    fn unknown_and_code_extensions_are_other() {
        for ext in ["rs", "js", "py", "dll", "log", "dat", "unknownext"] {
            assert_eq!(FileKind::from_extension(ext), FileKind::Other, "{ext}");
        }
    }

    #[test]
    fn from_path_extracts_extension_from_full_path() {
        assert_eq!(
            FileKind::from_path("C:\\Users\\Ada\\holiday.mp4"),
            FileKind::Video
        );
        assert_eq!(FileKind::from_path("/mnt/d/photo.JPG"), FileKind::Image);
    }

    #[test]
    fn multi_dot_uses_last_extension() {
        assert_eq!(FileKind::from_path("backup.tar.gz"), FileKind::Archive);
        assert_eq!(FileKind::from_path("archive.7z"), FileKind::Archive);
    }

    #[test]
    fn files_without_extension_and_dotfiles_are_other() {
        assert_eq!(FileKind::from_path("C:\\bin\\Makefile"), FileKind::Other);
        assert_eq!(FileKind::from_path(".gitignore"), FileKind::Other);
        assert_eq!(FileKind::from_path("C:\\repo\\.env"), FileKind::Other);
        assert_eq!(FileKind::from_path("noext"), FileKind::Other);
        assert_eq!(FileKind::from_path("trailingdot."), FileKind::Other);
    }

    #[test]
    fn decision_keep_hides_mark_is_reap() {
        // T-057: Keep ховає; Marked — reap bar.
        assert!(Decision::Keep.hides_from_candidates());
        assert!(!Decision::Keep.is_reclaimable());
        assert!(Decision::Marked.is_marked_for_reap());
        assert!(Decision::Marked.is_reclaimable());
        assert!(Decision::Undecided.is_reclaimable());
    }

    #[test]
    fn verdict_holds_category_explanation_and_safety() {
        // T-036 / architecture.md §6.2: три обов'язкові поля вердикту.
        let v = Verdict::new(
            CategoryId::ForgottenVideos,
            "відео 4.2 ГБ, останній доступ 8 міс тому",
            SafetyLevel::ReviewRecommended,
        );
        assert_eq!(v.category, CategoryId::ForgottenVideos);
        assert!(v.explanation.contains("4.2"));
        assert_eq!(v.safety, SafetyLevel::ReviewRecommended);
        assert!(v.is_complete());

        let incomplete = Verdict {
            category: CategoryId::Archives,
            explanation: "   ".into(),
            safety: SafetyLevel::SafeToBulk,
        };
        assert!(!incomplete.is_complete());
    }
}
