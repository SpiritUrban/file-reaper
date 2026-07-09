//! Форматування пояснень детекторів (UI-рядок вердикту).

/// Людський розмір для пояснення «Великі файли» тощо.
///
/// DoD T-039: «розмір N ГБ». Для файлів &lt; 1 ГіБ показуємо частку ГБ
/// (напр. «розмір 0.098 ГБ» для 100 МіБ), щоб формат був єдиним.
pub fn format_size_gb(bytes: u64) -> String {
    let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    if gb >= 100.0 {
        format!("розмір {:.0} ГБ", gb)
    } else if gb >= 10.0 {
        format!("розмір {:.1} ГБ", gb)
    } else if gb >= 1.0 {
        format!("розмір {:.2} ГБ", gb)
    } else {
        // < 1 ГБ (типовий поріг 100 МіБ) — три знаки після коми
        format!("розмір {:.3} ГБ", gb)
    }
}

/// Повний рядок пояснення T-039: «розмір N ГБ».
pub fn large_file_explanation(bytes: u64) -> String {
    format_size_gb(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_multi_gb() {
        let s = format_size_gb(5 * 1024 * 1024 * 1024);
        assert!(s.contains("ГБ"), "{s}");
        assert!(s.starts_with("розмір "), "{s}");
        // ~5 ГБ
        assert!(s.contains('5'), "{s}");
    }

    #[test]
    fn formats_sub_gb_as_fraction() {
        // 100 MiB ≈ 0.098 ГБ
        let s = format_size_gb(100 * 1024 * 1024);
        assert!(s.contains("ГБ"), "{s}");
        assert!(s.starts_with("розмір "), "{s}");
    }
}
