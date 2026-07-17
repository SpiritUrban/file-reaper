//! Декларативний реєстр відомих локацій (T-044).
//!
//! architecture.md §6 / repository.md: Temp і кеші програм — **дані**, не код.
//! Файл `registry/known-locations.json` завантажується з диска в рантаймі:
//! новий запис підхоплюється **без перекомпіляції** ядра (DoD T-044).
//!
//! Наповнення Temp — T-045; кеші ЦА — T-046; детектори — T-047/T-048.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trashradar_domain::candidate::SafetyLevel;
use trashradar_domain::error::{CoreError, ErrorCode};

/// Поточна підтримувана версія схеми `known-locations.json`.
pub const KNOWN_LOCATIONS_SCHEMA_VERSION: u32 = 1;

/// Ім'я канонічного файлу в каталозі `registry/`.
pub const KNOWN_LOCATIONS_FILE: &str = "known-locations.json";

/// Вшитий у бінарник дефолтний реєстр (той самий `registry/known-locations.json`)
/// — фолбек [`KnownLocationsRegistry::load_default_or_embedded`], коли теки
/// `registry/` немає поруч із застосунком (продакшн-бандл).
const EMBEDDED_KNOWN_LOCATIONS: &str =
    include_str!("../../../registry/known-locations.json");

/// До якого детектора/категорії належить локація.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationKind {
    /// Тимчасові файли (T-047).
    TempFiles,
    /// Кеші програм (T-048).
    AppCaches,
}

impl LocationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LocationKind::TempFiles => "temp_files",
            LocationKind::AppCaches => "app_caches",
        }
    }
}

/// Як зіставляти шлях кандидата з розгорнутим коренем локації.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PathMatchMode {
    /// Кандидат лежить **під** коренем (префікс сегментів шляху).
    #[default]
    Prefix,
}

/// Один запис реєстру (дані, не код).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationEntry {
    /// Стабільний id (`windows.temp.user`, `browser.chrome.cache`, …).
    pub id: String,
    /// temp_files | app_caches.
    pub kind: LocationKind,
    /// Рівень безпечності вердикту детектора.
    pub safety: SafetyLevel,
    /// Шаблони шляхів з плейсхолдерами `%VAR%` (див. [`expand_path_template`]).
    pub paths: Vec<String>,
    /// Режим зіставлення; за замовчуванням prefix.
    #[serde(default)]
    pub match_mode: PathMatchMode,
    /// Людське пояснення для UI (рядок вердикту / плитки).
    pub explanation: String,
    /// Опційний короткий ярлик для UI (якщо порожньо — `id`).
    #[serde(default)]
    pub label: Option<String>,
}

/// Кореневий документ `known-locations.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownLocationsDocument {
    /// Документаційний коментар (ігнорується кодом).
    #[serde(rename = "$comment", default, skip_serializing)]
    pub comment: Option<String>,
    pub schema_version: u32,
    #[serde(default)]
    pub locations: Vec<LocationEntry>,
}

/// Завантажений і провалідований реєстр.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownLocationsRegistry {
    pub schema_version: u32,
    pub locations: Vec<LocationEntry>,
    /// Звідки прочитано (для логів / health).
    pub source_path: Option<PathBuf>,
}

impl KnownLocationsRegistry {
    /// Розбір + валідація з JSON-рядка (без I/O).
    pub fn from_json_str(json: &str) -> Result<Self, CoreError> {
        let doc: KnownLocationsDocument = serde_json::from_str(json).map_err(|e| {
            CoreError::invalid_argument(format!("Реєстр локацій: некоректний JSON ({e})."))
        })?;
        Self::from_document(doc, None)
    }

    pub fn from_document(
        doc: KnownLocationsDocument,
        source_path: Option<PathBuf>,
    ) -> Result<Self, CoreError> {
        validate_document(&doc)?;
        Ok(Self {
            schema_version: doc.schema_version,
            locations: doc.locations,
            source_path,
        })
    }

    /// Прочитати файл з диска (runtime — без перекомпіляції).
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path).map_err(|e| {
            CoreError::new(
                ErrorCode::Io,
                format!(
                    "Не вдалося прочитати реєстр локацій «{}»: {e}.",
                    path.display()
                ),
            )
        })?;
        let mut reg = Self::from_json_str(&raw)?;
        reg.source_path = Some(path.to_path_buf());
        Ok(reg)
    }

    /// Пошук файлу реєстру: `TRASHRADAR_REGISTRY_DIR`, поруч з exe, `registry/` від CWD.
    pub fn load_default() -> Result<Self, CoreError> {
        let path = resolve_known_locations_path().ok_or_else(|| {
            CoreError::new(
                ErrorCode::Io,
                "Не знайдено known-locations.json (задайте TRASHRADAR_REGISTRY_DIR або покладіть registry/ поруч із застосунком).",
            )
        })?;
        Self::load_from_file(path)
    }

    /// Як [`Self::load_default`], але з вбудованим фолбеком: якщо файл реєстру
    /// не знайдено (продакшн-бандл без теки `registry/`), парсимо вшитий у
    /// бінарник дефолт. Файловий шлях має пріоритет — дозволяє оновлювати
    /// локації без ребілду. Так розділи «Тимчасові»/«Кеші» працюють і в dev,
    /// і в упакованому застосунку.
    pub fn load_default_or_embedded() -> Result<Self, CoreError> {
        match Self::load_default() {
            Ok(registry) => Ok(registry),
            Err(_) => Self::from_json_str(EMBEDDED_KNOWN_LOCATIONS),
        }
    }

    pub fn len(&self) -> usize {
        self.locations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&LocationEntry> {
        self.locations.iter().find(|e| e.id == id)
    }

    pub fn by_kind(&self, kind: LocationKind) -> impl Iterator<Item = &LocationEntry> {
        self.locations.iter().filter(move |e| e.kind == kind)
    }

    /// Усі `temp_files` з реєстру.
    pub fn temp_locations(&self) -> impl Iterator<Item = &LocationEntry> {
        self.by_kind(LocationKind::TempFiles)
    }

    /// Усі `app_caches` з реєстру (T-046).
    pub fn app_cache_locations(&self) -> impl Iterator<Item = &LocationEntry> {
        self.by_kind(LocationKind::AppCaches)
    }
}

impl LocationEntry {
    /// Розгорнути `paths` у абсолютні корені (невідомі `%VAR%` пропускаються).
    pub fn expanded_roots(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for template in &self.paths {
            if let Some(p) = expand_path_template(template) {
                let key = normalize_path_for_match(&p);
                if seen.insert(key) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// Розгорнуті корені, які **існують** як каталоги на цій машині.
    pub fn existing_roots(&self) -> Vec<PathBuf> {
        self.expanded_roots()
            .into_iter()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .collect()
    }
}

fn validate_document(doc: &KnownLocationsDocument) -> Result<(), CoreError> {
    if doc.schema_version != KNOWN_LOCATIONS_SCHEMA_VERSION {
        return Err(CoreError::invalid_argument(format!(
            "Реєстр локацій: schema_version {} не підтримується (очікується {}).",
            doc.schema_version, KNOWN_LOCATIONS_SCHEMA_VERSION
        )));
    }

    let mut seen = HashSet::new();
    for (i, entry) in doc.locations.iter().enumerate() {
        if entry.id.trim().is_empty() {
            return Err(CoreError::invalid_argument(format!(
                "Реєстр локацій: запис [{i}] має порожній id."
            )));
        }
        if !seen.insert(entry.id.as_str()) {
            return Err(CoreError::invalid_argument(format!(
                "Реєстр локацій: дублікат id «{}».",
                entry.id
            )));
        }
        if entry.paths.is_empty() {
            return Err(CoreError::invalid_argument(format!(
                "Реєстр локацій: «{}» не має жодного path.",
                entry.id
            )));
        }
        for (j, p) in entry.paths.iter().enumerate() {
            if p.trim().is_empty() {
                return Err(CoreError::invalid_argument(format!(
                    "Реєстр локацій: «{}».paths[{j}] порожній.",
                    entry.id
                )));
            }
        }
        if entry.explanation.trim().is_empty() {
            return Err(CoreError::invalid_argument(format!(
                "Реєстр локацій: «{}» без explanation.",
                entry.id
            )));
        }
    }
    Ok(())
}

/// Знайти `known-locations.json` на диску.
pub fn resolve_known_locations_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("TRASHRADAR_REGISTRY_DIR") {
        let p = PathBuf::from(dir).join(KNOWN_LOCATIONS_FILE);
        if p.is_file() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidates = [
                dir.join("registry").join(KNOWN_LOCATIONS_FILE),
                dir.join(KNOWN_LOCATIONS_FILE),
                // dev: target/debug → ../../../registry
                dir.join("..")
                    .join("..")
                    .join("..")
                    .join("registry")
                    .join(KNOWN_LOCATIONS_FILE),
            ];
            for c in candidates {
                if let Ok(canon) = c.canonicalize() {
                    if canon.is_file() {
                        return Some(canon);
                    }
                } else if c.is_file() {
                    return Some(c);
                }
            }
        }
    }

    // CWD / workspace root heuristics
    [
        Path::new("registry").join(KNOWN_LOCATIONS_FILE),
        Path::new("..").join("registry").join(KNOWN_LOCATIONS_FILE),
        Path::new("../..")
            .join("registry")
            .join(KNOWN_LOCATIONS_FILE),
    ]
    .into_iter()
    .find(|rel| rel.is_file())
}

/// Розгорнути шаблон шляху з `%VAR%` у абсолютний (Windows env).
///
/// Підтримувані змінні: `TEMP`, `TMP`, `LOCALAPPDATA`, `APPDATA`,
/// `USERPROFILE`, `WINDIR`, `SystemRoot`, `PROGRAMDATA`, `PROGRAMFILES`,
/// `PROGRAMFILES(X86)`, `HOMEDRIVE`, `HOMEPATH`.
///
/// Невідомий `%VAR%` → `None` (запис пропускається детектором, не паніка).
pub fn expand_path_template(template: &str) -> Option<String> {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(end) = template[i + 1..].find('%') {
                let name = &template[i + 1..i + 1 + end];
                if name.is_empty() {
                    out.push('%');
                    i += 1;
                    continue;
                }
                let val = std::env::var(name).ok()?;
                out.push_str(&val);
                i += name.len() + 2;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Чи `candidate_path` лежить під `root` (префікс сегментів, регістронезалежно на Windows).
pub fn path_matches_prefix(candidate_path: &str, root: &str) -> bool {
    let c = normalize_path_for_match(candidate_path);
    let mut r = normalize_path_for_match(root);
    while r.ends_with('\\') {
        r.pop();
    }
    if r.is_empty() {
        return false;
    }
    c == r || c.starts_with(&(r.clone() + "\\"))
}

fn normalize_path_for_match(p: &str) -> String {
    let mut s: String = p
        .chars()
        .map(|ch| {
            if ch == '/' {
                '\\'
            } else {
                ch.to_ascii_lowercase()
            }
        })
        .collect();
    while s.ends_with('\\') {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_OK: &str = r#"{
      "schema_version": 1,
      "locations": [
        {
          "id": "test.temp.sample",
          "kind": "temp_files",
          "safety": "safe_to_bulk",
          "paths": ["%TEMP%\\TrashRadarTest", "C:\\Temp\\Sample"],
          "match_mode": "prefix",
          "explanation": "тестова temp-локація",
          "label": "Test Temp"
        }
      ]
    }"#;

    #[test]
    fn parses_valid_document() {
        // Вшитий дефолт має парситись і містити temp/caches локації — інакше
        // «Тимчасові/Кеші» мовчки порожні в продакшн-бандлі без registry/.
        let embedded = KnownLocationsRegistry::from_json_str(EMBEDDED_KNOWN_LOCATIONS)
            .expect("вбудований known-locations.json валідний");
        assert!(!embedded.is_empty(), "вбудований реєстр не порожній");
        assert!(KnownLocationsRegistry::load_default_or_embedded().is_ok());

        let reg = KnownLocationsRegistry::from_json_str(MINIMAL_OK).expect("parse");
        assert_eq!(reg.schema_version, 1);
        assert_eq!(reg.len(), 1);
        let e = reg.get("test.temp.sample").unwrap();
        assert_eq!(e.kind, LocationKind::TempFiles);
        assert_eq!(e.safety, SafetyLevel::SafeToBulk);
        assert_eq!(e.paths.len(), 2);
        assert_eq!(e.match_mode, PathMatchMode::Prefix);
    }

    #[test]
    fn new_entry_picked_up_without_code_change() {
        // DoD T-044: новий запис — лише дані JSON, без змін Rust.
        let with_second = r#"{
          "schema_version": 1,
          "locations": [
            {
              "id": "a.one",
              "kind": "temp_files",
              "safety": "safe_to_bulk",
              "paths": ["%TEMP%"],
              "explanation": "one"
            },
            {
              "id": "b.two",
              "kind": "app_caches",
              "safety": "safe_to_bulk",
              "paths": ["%LOCALAPPDATA%\\Foo\\Cache"],
              "explanation": "two"
            }
          ]
        }"#;
        let reg = KnownLocationsRegistry::from_json_str(with_second).unwrap();
        assert_eq!(reg.len(), 2);
        assert!(reg.get("b.two").is_some());
        assert_eq!(reg.by_kind(LocationKind::AppCaches).count(), 1);
    }

    #[test]
    fn rejects_wrong_schema_version() {
        let j = r#"{"schema_version": 0, "locations": []}"#;
        let err = KnownLocationsRegistry::from_json_str(j).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("schema_version"));
    }

    #[test]
    fn rejects_duplicate_ids() {
        let j = r#"{
          "schema_version": 1,
          "locations": [
            {"id":"x","kind":"temp_files","safety":"safe_to_bulk","paths":["%TEMP%"],"explanation":"a"},
            {"id":"x","kind":"temp_files","safety":"safe_to_bulk","paths":["%TMP%"],"explanation":"b"}
          ]
        }"#;
        let err = KnownLocationsRegistry::from_json_str(j).unwrap_err();
        assert!(err.message.contains("дублікат"));
    }

    #[test]
    fn rejects_empty_paths_or_explanation() {
        let j = r#"{
          "schema_version": 1,
          "locations": [
            {"id":"x","kind":"temp_files","safety":"safe_to_bulk","paths":[],"explanation":"a"}
          ]
        }"#;
        assert!(KnownLocationsRegistry::from_json_str(j).is_err());

        let j2 = r#"{
          "schema_version": 1,
          "locations": [
            {"id":"x","kind":"temp_files","safety":"safe_to_bulk","paths":["%TEMP%"],"explanation":"  "}
          ]
        }"#;
        assert!(KnownLocationsRegistry::from_json_str(j2).is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        let j = r#"{
          "schema_version": 1,
          "locations": [
            {"id":"x","kind":"temp_files","safety":"safe_to_bulk","paths":["%TEMP%"],"explanation":"a","oops":true}
          ]
        }"#;
        assert!(KnownLocationsRegistry::from_json_str(j).is_err());
    }

    #[test]
    fn path_prefix_match_is_segment_safe() {
        assert!(path_matches_prefix(
            r"C:\Users\Ada\AppData\Local\Temp\foo.txt",
            r"C:\Users\Ada\AppData\Local\Temp"
        ));
        assert!(path_matches_prefix(
            r"C:\Users\Ada\AppData\Local\Temp",
            r"C:\Users\Ada\AppData\Local\Temp"
        ));
        // не префікс сегмента
        assert!(!path_matches_prefix(
            r"C:\Users\Ada\AppData\Local\Temp2\x",
            r"C:\Users\Ada\AppData\Local\Temp"
        ));
    }

    #[test]
    fn expand_literal_path_without_placeholders() {
        let p = expand_path_template(r"C:\Temp\Sample").unwrap();
        assert_eq!(p, r"C:\Temp\Sample");
    }

    #[test]
    fn expand_unknown_env_returns_none() {
        assert!(expand_path_template("%TRASHRADAR_NO_SUCH_VAR_XYZ%\\x").is_none());
    }

    fn workspace_registry_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("registry")
            .join(KNOWN_LOCATIONS_FILE)
    }

    #[test]
    fn loads_workspace_registry_file_if_present() {
        let path = workspace_registry_path();
        if !path.is_file() {
            return;
        }
        let reg = KnownLocationsRegistry::load_from_file(&path).expect("workspace registry");
        assert_eq!(reg.schema_version, KNOWN_LOCATIONS_SCHEMA_VERSION);
        assert!(reg.locations.iter().all(|e| !e.id.is_empty()));
    }

    /// DoD T-045: стандартні Temp-локації є в реєстрі й резолвляться на Windows.
    #[test]
    fn t045_temp_entries_present_in_registry() {
        let path = workspace_registry_path();
        if !path.is_file() {
            return;
        }
        let reg = KnownLocationsRegistry::load_from_file(&path).expect("load");
        let temps: Vec<_> = reg.temp_locations().collect();
        assert!(
            !temps.is_empty(),
            "T-045: очікували ≥1 temp_files у known-locations.json"
        );

        let ids: HashSet<_> = temps.iter().map(|e| e.id.as_str()).collect();
        assert!(
            ids.contains("windows.temp.user"),
            "обов'язковий windows.temp.user: {ids:?}"
        );
        assert!(
            ids.contains("windows.temp.system"),
            "обов'язковий windows.temp.system: {ids:?}"
        );

        for e in &temps {
            assert_eq!(e.kind, LocationKind::TempFiles);
            assert_eq!(
                e.safety,
                SafetyLevel::SafeToBulk,
                "{} має бути safe_to_bulk",
                e.id
            );
            assert!(!e.paths.is_empty());
        }
    }

    #[test]
    #[cfg(windows)]
    fn t045_standard_temp_dirs_exist_on_clean_windows() {
        // DoD: «Стандартні Temp-локації знаходяться на чистій інсталяції Windows».
        let path = workspace_registry_path();
        if !path.is_file() {
            return;
        }
        let reg = KnownLocationsRegistry::load_from_file(&path).expect("load");

        let user = reg.get("windows.temp.user").expect("user temp entry");
        let user_roots = user.existing_roots();
        assert!(
            !user_roots.is_empty(),
            "User Temp (%TEMP% / Local\\Temp) має існувати; expanded={:?}",
            user.expanded_roots()
        );

        let system = reg.get("windows.temp.system").expect("system temp entry");
        let system_roots = system.existing_roots();
        assert!(
            !system_roots.is_empty(),
            "Windows\\Temp має існувати; expanded={:?}",
            system.expanded_roots()
        );

        // Хоча б один шлях з user збігається з env TEMP (типова чиста інсталяція).
        if let Ok(temp) = std::env::var("TEMP") {
            let temp_n = normalize_path_for_match(&temp);
            let hit = user_roots.iter().any(|r| {
                let rn = normalize_path_for_match(&r.to_string_lossy());
                rn == temp_n
                    || temp_n.starts_with(&(rn.clone() + "\\"))
                    || rn.starts_with(&(temp_n.clone() + "\\"))
            });
            assert!(
                hit,
                "%TEMP%={temp} має покриватись windows.temp.user roots={user_roots:?}"
            );
        }
    }

    /// DoD T-046: ≥20 записів app_caches, валідна схема, покриття сегментів ЦА.
    #[test]
    fn t046_app_cache_catalog_present() {
        let path = workspace_registry_path();
        if !path.is_file() {
            return;
        }
        let reg = KnownLocationsRegistry::load_from_file(&path).expect("load");
        let caches: Vec<_> = reg.app_cache_locations().collect();
        assert!(
            caches.len() >= 20,
            "T-046: очікували ≥20 app_caches, є {}",
            caches.len()
        );

        let ids: HashSet<_> = caches.iter().map(|e| e.id.as_str()).collect();
        for required in [
            "browser.chrome.cache",
            "browser.edge.cache",
            "ide.vscode.cache",
            "pkg.npm.cache",
            "pkg.pip.cache",
            "pkg.cargo.registry",
            "messenger.discord.cache",
            "render.adobe.media_cache",
            "game.steam.htmlcache",
        ] {
            assert!(ids.contains(required), "немає {required} у {ids:?}");
        }

        for e in &caches {
            assert_eq!(e.kind, LocationKind::AppCaches);
            assert!(!e.paths.is_empty(), "{} без paths", e.id);
            assert!(!e.explanation.trim().is_empty(), "{} без explanation", e.id);
            for p in &e.paths {
                assert!(
                    p.contains('%') || p.chars().nth(1) == Some(':'),
                    "{} path «{p}» виглядає невалідно",
                    e.id
                );
            }
        }
    }

    /// Live probe: для встановленого ПЗ existing_roots непорожній.
    /// Не вимагаємо всі 20 на CI — лише узгодженість шаблонів з FS.
    #[test]
    #[cfg(windows)]
    fn t046_live_probe_installed_app_caches() {
        let path = workspace_registry_path();
        if !path.is_file() {
            return;
        }
        let reg = KnownLocationsRegistry::load_from_file(&path).expect("load");

        let mut present = Vec::new();
        let mut absent = Vec::new();
        for e in reg.app_cache_locations() {
            let roots = e.existing_roots();
            if roots.is_empty() {
                absent.push(e.id.as_str());
            } else {
                present.push((e.id.as_str(), roots));
            }
        }

        println!(
            "T-046 live probe: {} present / {} absent (of {})",
            present.len(),
            absent.len(),
            present.len() + absent.len()
        );
        for (id, roots) in &present {
            println!("  OK  {id}: {roots:?}");
        }
        for id in &absent {
            println!("  --  {id}: not installed / path missing");
        }

        for (id, roots) in &present {
            assert!(
                roots.iter().all(|r| r.is_dir()),
                "{id}: existing_roots має бути dir"
            );
        }
    }
}
