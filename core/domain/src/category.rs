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
    /// Рекурсивно порожні папки (жодного файла в усьому піддереві) — показуємо
    /// найвищу порожню папку, видалення прибирає всю порожню гілку (folder-unit).
    EmptyFolders,
    /// Папки з дуже малою рекурсивною кількістю файлів (1..=поріг, дефолт 3) —
    /// кандидати на прибирання зайвого ускладнення структури (folder-unit).
    SparseFolders,
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
    pub const ALL: [CategoryId; 11] = [
        CategoryId::LargeFiles,
        CategoryId::OldFiles,
        CategoryId::ForgottenVideos,
        CategoryId::Duplicates,
        CategoryId::Archives,
        CategoryId::Installers,
        CategoryId::TempFiles,
        CategoryId::AppCaches,
        CategoryId::DevArtifacts,
        CategoryId::EmptyFolders,
        CategoryId::SparseFolders,
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
            CategoryId::EmptyFolders => Some(9),
            CategoryId::SparseFolders => Some(10),
            CategoryId::Uncategorized => None,
        }
    }
}

/// Бітова маска категорій-детекторів (T-121: маркер перетину «також у: …»).
/// Біт `i` = присутність `CategoryId::ALL[i]`; `Uncategorized` не кодується
/// (не входить у [`CategoryId::ALL`]/[`CategoryId::mvp_index`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CategoryMask(pub u16);

impl CategoryMask {
    pub const EMPTY: CategoryMask = CategoryMask(0);

    pub fn insert(&mut self, category: CategoryId) {
        if let Some(index) = category.mvp_index() {
            self.0 |= 1 << index;
        }
    }

    pub fn contains(self, category: CategoryId) -> bool {
        category
            .mvp_index()
            .is_some_and(|index| self.0 & (1 << index) != 0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Категорії маски за винятком `exclude` (типово — власна категорія
    /// плитки: «також у: …» не повторює поточну категорію).
    pub fn iter_excluding(self, exclude: CategoryId) -> impl Iterator<Item = CategoryId> {
        CategoryId::ALL
            .into_iter()
            .filter(move |category| *category != exclude && self.contains(*category))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_mask_insert_contains_roundtrip() {
        let mut mask = CategoryMask::EMPTY;
        assert!(mask.is_empty());
        mask.insert(CategoryId::Duplicates);
        mask.insert(CategoryId::OldFiles);
        assert!(mask.contains(CategoryId::Duplicates));
        assert!(mask.contains(CategoryId::OldFiles));
        assert!(!mask.contains(CategoryId::LargeFiles));
        assert!(!mask.is_empty());
    }

    #[test]
    fn category_mask_iter_excluding_skips_primary() {
        let mut mask = CategoryMask::EMPTY;
        mask.insert(CategoryId::OldFiles);
        mask.insert(CategoryId::Duplicates);
        mask.insert(CategoryId::ForgottenVideos);
        let others: Vec<_> = mask.iter_excluding(CategoryId::OldFiles).collect();
        assert_eq!(
            others,
            vec![CategoryId::ForgottenVideos, CategoryId::Duplicates]
        );
    }

    #[test]
    fn category_mask_uncategorized_is_never_set() {
        let mut mask = CategoryMask::EMPTY;
        mask.insert(CategoryId::Uncategorized);
        assert!(mask.is_empty(), "Uncategorized не кодується маскою");
    }
}
