//! Нормалізація шляхів для порівнянь і ключів (правило 6a брифу Стадії 2).
//!
//! # Навіщо окремий модуль
//!
//! По кодовій базі було розсіяно вісім копій одного й того самого:
//! `path.replace('/', "\\").to_ascii_lowercase()`. Кожна компілюється на
//! будь-якій платформі й проходить clippy — а працює лише на Windows:
//!
//! - на Unix `\` — **легальний символ імені файла**, тож `/tmp/x` стає
//!   неіснуючим `\tmp\x`, і кожна операція копіювання чи переміщення
//!   повідомляє «файл зник»;
//! - Linux **регістрозалежний**, тож `Foo.JPG` і `foo.jpg` — різні файли, а
//!   беззастережний `to_ascii_lowercase()` зливає їх в один запис індексу.
//!
//! Компіляція про це не скаже нічого. Тому рішення «який роздільник» і
//! «чи згортати регістр» ухвалюється рівно тут, один раз.

/// Роздільник шляхів поточної платформи.
pub const SEPARATOR: char = if cfg!(windows) { '\\' } else { '/' };

/// Чужий роздільник, який нормалізація зводить до [`SEPARATOR`].
///
/// На Windows це `/` (обидва легальні, ядро приймає обидва). На Unix
/// зворотного перетворення НЕМАЄ: `\` там — звичайний символ імені.
const FOREIGN_SEPARATOR: Option<char> = if cfg!(windows) { Some('/') } else { None };

/// Чи регістронезалежна файлова система платформи.
///
/// Windows і macOS (APFS за замовчуванням) — так, Linux — ні.
pub const CASE_INSENSITIVE: bool = cfg!(windows) || cfg!(target_os = "macos");

/// Звести роздільники до платформного, не чіпаючи регістр.
pub fn normalize_separators(path: &str) -> String {
    match FOREIGN_SEPARATOR {
        Some(foreign) => path.replace(foreign, &SEPARATOR.to_string()),
        None => path.to_string(),
    }
}

/// Згорнути регістр там, де файлова система його не розрізняє.
pub fn fold_case(path: &str) -> String {
    if CASE_INSENSITIVE {
        path.to_ascii_lowercase()
    } else {
        path.to_string()
    }
}

/// Канонічний ключ шляху: роздільники + регістр за правилами платформи.
///
/// Саме ним порівнюються шляхи в індексі, кеші хешів і guard-list'ах.
pub fn path_key(path: &str) -> String {
    fold_case(&normalize_separators(path))
}

/// Порівняння двох шляхів за правилами платформи.
pub fn paths_equal(a: &str, b: &str) -> bool {
    if CASE_INSENSITIVE {
        normalize_separators(a).eq_ignore_ascii_case(&normalize_separators(b))
    } else {
        normalize_separators(a) == normalize_separators(b)
    }
}

/// Прибрати хвостові роздільники (обидва варіанти — вхід буває чужий).
pub fn trim_trailing_separators(path: &str) -> &str {
    path.trim_end_matches(['\\', '/'])
}

/// Приєднати ім'я дитини до батьківського шляху платформним роздільником.
pub fn join_child(parent: &str, name: &str) -> String {
    format!("{}{}{}", trim_trailing_separators(parent), SEPARATOR, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Очікуване будується з [`SEPARATOR`], а не хардкодиться рядком: тест з
    /// `assert_eq!(got, "c:\\a\\b")` перевіряв би роздільник, а не логіку, і
    /// був би зеленим рівно на одній ОС (правило 6a).
    #[test]
    fn separators_are_platform_native() {
        let joined = join_child("parent", "child");
        assert_eq!(joined, format!("parent{SEPARATOR}child"));
        assert_eq!(join_child("parent/", "child"), joined);
        assert_eq!(join_child("parent\\", "child"), joined);
    }

    #[test]
    fn foreign_separator_converted_only_where_it_is_foreign() {
        let got = normalize_separators("a/b");
        if cfg!(windows) {
            assert_eq!(got, "a\\b");
        } else {
            // На Unix `/` — рідний, а `\` у імені файла легальний і чіпати
            // його не можна.
            assert_eq!(got, "a/b");
            assert_eq!(normalize_separators("a\\b"), "a\\b");
        }
    }

    #[test]
    fn case_folding_follows_the_filesystem() {
        let got = path_key("Foo.JPG");
        if CASE_INSENSITIVE {
            assert_eq!(got, "foo.jpg");
            assert!(paths_equal("Foo.JPG", "foo.jpg"));
        } else {
            assert_eq!(got, "Foo.JPG");
            assert!(!paths_equal("Foo.JPG", "foo.jpg"));
        }
    }

    #[test]
    fn trailing_separators_trimmed_from_both_forms() {
        assert_eq!(trim_trailing_separators("a/b/"), "a/b");
        assert_eq!(trim_trailing_separators("a\\b\\"), "a\\b");
        assert_eq!(trim_trailing_separators("a"), "a");
    }
}
