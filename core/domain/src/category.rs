//! Категорії-детектори MVP (docs/product.md §5.3).

use serde::{Deserialize, Serialize};

/// Дев'ять категорій MVP. Порядок тут не визначає порядок у UI —
/// Sidebar сортує за фактичним обсягом (docs/ui.md §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryId {
    LargeFiles,
    OldFiles,
    ForgottenVideos,
    Duplicates,
    Archives,
    Installers,
    TempFiles,
    AppCaches,
    DevArtifacts,
}

impl CategoryId {
    /// Повний перелік — для реєстрації детекторів і UI.
    pub const ALL: [CategoryId; 9] = [
        CategoryId::LargeFiles,
        CategoryId::OldFiles,
        CategoryId::ForgottenVideos,
        CategoryId::Duplicates,
        CategoryId::Archives,
        CategoryId::Installers,
        CategoryId::TempFiles,
        CategoryId::AppCaches,
        CategoryId::DevArtifacts,
    ];
}
