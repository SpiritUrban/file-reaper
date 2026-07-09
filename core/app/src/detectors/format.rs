//! Форматування пояснень детекторів (UI-рядок вердикту).

/// FILETIME ticks (100 ns) per calendar day.
pub const FILETIME_PER_DAY: i64 = 10_000_000 * 86_400;
/// Секунди між 1601-01-01 і 1970-01-01 (Unix epoch у FILETIME).
const UNIX_EPOCH_AS_FILETIME_SECS: i64 = 11_644_473_600;

/// Поточний час як Windows FILETIME (для вікових детекторів).
pub fn system_now_filetime() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (secs + UNIX_EPOCH_AS_FILETIME_SECS) * 10_000_000
}

/// Вік у повних днях між `ts` і `now` (FILETIME). Якщо ts ≥ now → 0.
pub fn age_days_filetime(now: i64, ts: i64) -> u64 {
    if ts >= now {
        0
    } else {
        ((now - ts) / FILETIME_PER_DAY) as u64
    }
}

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

/// Стислий вік українською: «14 дн.», «3 міс.», «2 р.».
pub fn format_age_uk(days: u64) -> String {
    if days >= 365 {
        let years = days / 365;
        if years == 1 {
            "1 р.".into()
        } else {
            format!("{years} р.")
        }
    } else if days >= 30 {
        let months = days / 30;
        if months == 1 {
            "1 міс.".into()
        } else {
            format!("{months} міс.")
        }
    } else if days == 1 {
        "1 дн.".into()
    } else {
        format!("{days} дн.")
    }
}

/// Пояснення T-040: доступ або зміна + давність.
pub fn old_file_explanation(age_days: u64, from_access: bool) -> String {
    let age = format_age_uk(age_days);
    if from_access {
        format!("останній доступ {age} тому")
    } else {
        format!("остання зміна {age} тому")
    }
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

    #[test]
    fn formats_age_units() {
        assert_eq!(format_age_uk(14), "14 дн.");
        assert_eq!(format_age_uk(45), "1 міс.");
        assert_eq!(format_age_uk(90), "3 міс.");
        assert_eq!(format_age_uk(400), "1 р.");
        assert_eq!(format_age_uk(800), "2 р.");
    }

    #[test]
    fn age_days_filetime_basic() {
        let now = 1_000_000_000_000_000i64;
        assert_eq!(age_days_filetime(now, now), 0);
        assert_eq!(age_days_filetime(now, now - FILETIME_PER_DAY * 10), 10);
    }
}
