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
    /// Скановано, але ще не заявлено жодним детектором (architecture.md §3.4:
    /// «знайдений сканером → у пам'яті → категоризований»). Такий запис живе
    /// лише в гарячому in-memory індексі й не показується користувачу; у
    /// persistent-SQLite потрапляють лише категоризовані записи. Свідомо НЕ
    /// входить до [`CategoryId::ALL`] — це не детекторна категорія.
    Uncategorized,
}

impl CategoryId {
    /// Повний перелік — для реєстрації детекторів і UI.
    ///
    /// Порядок збігається з [`crate::aggregate::FreeableSummary::by_category`]
    /// / [`CategoryId::mvp_index`].
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

    /// Індекс у [`CategoryId::ALL`] / `FreeableSummary::by_category` (T-054).
    pub fn mvp_index(self) -> Option<usize> {
        match self {
            CategoryId::LargeFiles => Some(0),
            CategoryId::OldFiles => Some(1),
            CategoryId::ForgottenVideos => Some(2),
            CategoryId::Duplicates => Some(3),
            CategoryId::Archives => Some(4),
            CategoryId::Installers => Some(5),
            CategoryId::TempFiles => Some(6),
            CategoryId::AppCaches => Some(7),
            CategoryId::DevArtifacts => Some(8),
            CategoryId::Uncategorized => None,
        }
    }
}
