//! Відсікання виключених шляхів на рівні обходу (T-027).
//!
//! architecture.md §2.2: у виключене піддерево сканер **взагалі не заходить**
//! (жоден `read_dir` / `metadata` на самому виключенні та його нащадках).
//! Список приходить з налаштувань (T-090/T-151) або системних дефолтів;
//! цей модуль — чиста перевірка префіксів, без I/O.

use std::path::Path;

/// Набір виключених шляхів (префікси піддерев).
///
/// Порівняння регістронезалежне (Windows); роздільники `/` і `\` еквівалентні;
/// хвостовий сепаратор ігнорується. `C:\Windows` виключає й
/// `C:\Windows\System32\…`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathExclusions {
    /// Нормалізовані префікси (lower-case, `\`, без хвостового `\`, окрім кореня `x:`).
    prefixes: Vec<String>,
}

impl PathExclusions {
    /// Порожній набір — нічого не виключається.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Зі списку шляхів (дублікати зливаються).
    pub fn from_paths<I, P>(paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut prefixes: Vec<String> = paths
            .into_iter()
            .filter_map(|p| normalize_prefix(p.as_ref()))
            .collect();
        prefixes.sort_unstable();
        prefixes.dedup();
        Self { prefixes }
    }

    /// Типові системні виключення Windows для тому `drive` (ui.md §9 / product).
    /// Повний список користувача зливається з цим у Settings (T-090); тут —
    /// лише вбудований мінімум для безпечного дефолту обходу.
    pub fn windows_system_defaults(drive: char) -> Self {
        if !drive.is_ascii_alphabetic() {
            return Self::empty();
        }
        let d = drive.to_ascii_uppercase();
        Self::from_paths([
            format!(r"{d}:\Windows"),
            format!(r"{d}:\Program Files"),
            format!(r"{d}:\Program Files (x86)"),
            format!(r"{d}:\ProgramData"),
            format!(r"{d}:\$Recycle.Bin"),
            format!(r"{d}:\System Volume Information"),
            format!(r"{d}:\Recovery"),
            format!(r"{d}:\Config.Msi"),
        ])
    }

    /// Додати шляхи (in-place).
    pub fn extend<I, P>(&mut self, paths: I)
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        for p in paths {
            if let Some(n) = normalize_prefix(p.as_ref()) {
                self.prefixes.push(n);
            }
        }
        self.prefixes.sort_unstable();
        self.prefixes.dedup();
    }

    pub fn is_empty(&self) -> bool {
        self.prefixes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.prefixes.len()
    }

    /// Ітератор нормалізованих префіксів (для діагностики/UI).
    pub fn prefixes(&self) -> impl Iterator<Item = &str> {
        self.prefixes.iter().map(String::as_str)
    }

    /// Чи потрапляє `path` під будь-яке виключення (сам префікс або нащадок).
    pub fn is_excluded(&self, path: &Path) -> bool {
        if self.prefixes.is_empty() {
            return false;
        }
        let Some(n) = normalize_prefix(path) else {
            return false;
        };
        self.prefixes.iter().any(|p| path_is_under(&n, p))
    }
}

/// Нормалізує шлях до канонічного префікса для порівняння.
fn normalize_prefix(path: &Path) -> Option<String> {
    let raw = path.to_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let mut s = raw.replace('/', "\\").to_ascii_lowercase();
    // Згортаємо повторні сепаратори.
    while s.contains("\\\\") {
        s = s.replace("\\\\", "\\");
    }
    // Прибираємо хвостовий `\` (але лишаємо `c:` без сепаратора — корінь тома
    // як префікс виключення означав би «весь том»; нормалізуємо `c:\` → `c:`).
    while s.ends_with('\\') {
        s.pop();
    }
    if s.is_empty() {
        return None;
    }
    Some(s)
}

/// `path` збігається з `prefix` або є його нащадком (`prefix\...`).
fn path_is_under(path: &str, prefix: &str) -> bool {
    if path == prefix {
        return true;
    }
    // `prefix\` + rest — уникаємо хибного `c:\win` ⊇ `c:\windows`.
    path.len() > prefix.len()
        && path.as_bytes().get(prefix.len()) == Some(&b'\\')
        && path.starts_with(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_descendant_match() {
        let ex = PathExclusions::from_paths([r"C:\Windows", r"D:\Skip"]);
        assert!(ex.is_excluded(Path::new(r"C:\Windows")));
        assert!(ex.is_excluded(Path::new(r"c:\windows\System32\drivers")));
        assert!(ex.is_excluded(Path::new(r"C:/Windows/Fonts")));
        assert!(!ex.is_excluded(Path::new(r"C:\Users")));
        assert!(!ex.is_excluded(Path::new(r"C:\WindowsBackup"))); // не префікс-сегмент
        assert!(ex.is_excluded(Path::new(r"d:\skip\deep\file.txt")));
    }

    #[test]
    fn trailing_slash_and_case_ignored() {
        let ex = PathExclusions::from_paths([r"C:\Program Files\"]);
        assert!(ex.is_excluded(Path::new(r"c:\program files")));
        assert!(ex.is_excluded(Path::new(r"C:\Program Files\Common Files")));
    }

    #[test]
    fn empty_excludes_nothing() {
        let ex = PathExclusions::empty();
        assert!(!ex.is_excluded(Path::new(r"C:\Windows")));
        assert!(ex.is_empty());
    }

    #[test]
    fn system_defaults_cover_windows_dir() {
        let ex = PathExclusions::windows_system_defaults('C');
        assert!(ex.is_excluded(Path::new(r"C:\Windows\System32")));
        assert!(ex.is_excluded(Path::new(r"C:\Program Files (x86)\Foo")));
        assert!(!ex.is_excluded(Path::new(r"C:\Users\Ada")));
    }

    #[test]
    fn extend_merges_and_dedups() {
        let mut ex = PathExclusions::from_paths([r"C:\A"]);
        ex.extend([r"C:\A\", r"C:\B"]);
        assert_eq!(ex.len(), 2);
    }
}
