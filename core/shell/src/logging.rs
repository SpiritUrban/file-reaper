//! Логування Core (T-003).
//!
//! Політика:
//! - файл у профілі користувача: `%LOCALAPPDATA%\TrashRadar\logs\core.log`
//!   (Local, не Roaming — логи не мають синхронізуватися між машинами;
//!   консолідація розташувань профілю — T-160);
//! - ротація за розміром: понад [`MAX_LOG_BYTES`] — файл зсувається у
//!   `core.1.log` … `core.N.log`, найстарший видаляється;
//! - рівні — через змінну середовища `TRASHRADAR_LOG` (синтаксис
//!   EnvFilter, напр. `debug` або `trashradar_shell=trace,info`);
//!   без змінної — [`DEFAULT_FILTER`];
//! - збій ініціалізації НЕ валить застосунок — деградація до stderr;
//! - паніки потрапляють у лог до стандартного обробника.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Стеля розміру активного файла лога.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
/// Скільки ротованих файлів зберігати (core.1.log … core.3.log).
const MAX_BACKUPS: usize = 3;
/// Фільтр за замовчуванням, коли `TRASHRADAR_LOG` не задано.
const DEFAULT_FILTER: &str = "info";

/// Ініціалізує глобальний підписник логів. Повертає шлях до файла лога.
///
/// Викликається один раз на старті `main`, до створення Tauri Builder.
pub fn init() -> Result<PathBuf, String> {
    let dir = default_log_dir()
        .ok_or_else(|| "змінна LOCALAPPDATA недоступна — каталог логів невідомий".to_string())?;
    fs::create_dir_all(&dir)
        .map_err(|e| format!("не вдалося створити каталог логів {}: {e}", dir.display()))?;

    let path = dir.join("core.log");
    let writer = RotatingWriter::open(&path, MAX_LOG_BYTES, MAX_BACKUPS)
        .map_err(|e| format!("не вдалося відкрити файл лога {}: {e}", path.display()))?;

    let filter = EnvFilter::try_from_env("TRASHRADAR_LOG")
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    let file_layer = fmt::layer().with_writer(writer).with_ansi(false);
    // У debug-збірці дублюємо у stderr — зручно при `tauri dev`.
    let stderr_layer = if cfg!(debug_assertions) {
        Some(fmt::layer().with_writer(io::stderr).with_ansi(false))
    } else {
        None
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(|e| format!("підписник логів уже встановлено: {e}"))?;

    log_panics();
    Ok(path)
}

/// `%LOCALAPPDATA%\TrashRadar\logs`.
fn default_log_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|base| PathBuf::from(base).join("TrashRadar").join("logs"))
}

/// Паніки — у лог (з тим самим форматом), потім стандартний обробник.
fn log_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(target: "panic", "{info}");
        previous(info);
    }));
}

/// Записувач з ротацією за розміром.
///
/// Потокобезпечний (`Mutex`), тому один екземпляр обслуговує всі потоки
/// підписника. Ротація: `core.log` → `core.1.log` → … → `core.N.log`,
/// файл понад ліміт зсуває ланцюжок, найстарший бекап видаляється.
struct RotatingWriter {
    path: PathBuf,
    max_bytes: u64,
    max_backups: usize,
    inner: Mutex<Inner>,
}

struct Inner {
    // Option: на Windows перейменувати відкритий файл не можна,
    // тому на час ротації дескриптор звільняється через take().
    file: Option<File>,
    len: u64,
}

impl RotatingWriter {
    fn open(path: &Path, max_bytes: u64, max_backups: usize) -> io::Result<Self> {
        let file = open_append(path)?;
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path: path.to_path_buf(),
            max_bytes,
            max_backups,
            inner: Mutex::new(Inner {
                file: Some(file),
                len,
            }),
        })
    }

    fn backup_path(&self, index: usize) -> PathBuf {
        self.path.with_extension(format!("{index}.log"))
    }

    fn rotate(&self, inner: &mut Inner) -> io::Result<()> {
        if let Some(mut file) = inner.file.take() {
            let _ = file.flush();
        } // drop → дескриптор звільнено, файл можна перейменувати

        if self.max_backups == 0 {
            let _ = fs::remove_file(&self.path);
        } else {
            let _ = fs::remove_file(self.backup_path(self.max_backups));
            for i in (1..self.max_backups).rev() {
                let _ = fs::rename(self.backup_path(i), self.backup_path(i + 1));
            }
            let _ = fs::rename(&self.path, self.backup_path(1));
        }

        let file = open_append(&self.path)?;
        inner.len = file.metadata().map(|m| m.len()).unwrap_or(0);
        inner.file = Some(file);
        Ok(())
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

impl io::Write for &RotatingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.len > 0 && inner.len + buf.len() as u64 > self.max_bytes {
            self.rotate(&mut inner)?;
        }
        let file = inner
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("файл лога недоступний після збою ротації"))?;
        let written = file.write(buf)?;
        inner.len += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match inner.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

impl<'a> fmt::MakeWriter<'a> for RotatingWriter {
    type Writer = &'a RotatingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Унікальний тимчасовий каталог на тест (без зовнішніх залежностей).
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("годинник")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("trashradar-logtest-{tag}-{nanos}"));
        fs::create_dir_all(&dir).expect("створення тимчасового каталогу");
        dir
    }

    fn write_line(writer: &RotatingWriter, len: usize) {
        let line = "x".repeat(len - 1) + "\n";
        (&mut &*writer)
            .write_all(line.as_bytes())
            .expect("запис у лог");
    }

    #[test]
    fn writes_to_current_file() {
        let dir = temp_dir("write");
        let path = dir.join("core.log");
        let writer = RotatingWriter::open(&path, 1024, 3).expect("відкриття");
        write_line(&writer, 10);
        let content = fs::read_to_string(&path).expect("читання");
        assert_eq!(content.len(), 10);
    }

    #[test]
    fn rotates_when_size_exceeded() {
        let dir = temp_dir("rotate");
        let path = dir.join("core.log");
        let writer = RotatingWriter::open(&path, 64, 3).expect("відкриття");
        for _ in 0..5 {
            write_line(&writer, 40); // 2 рядки > 64 байт → ротація
        }
        assert!(path.exists(), "активний файл існує");
        assert!(dir.join("core.1.log").exists(), "перший бекап існує");
        let current = fs::metadata(&path).expect("метадані").len();
        assert!(current <= 64, "активний файл у межах ліміту: {current}");
    }

    #[test]
    fn keeps_at_most_max_backups() {
        let dir = temp_dir("cap");
        let path = dir.join("core.log");
        let writer = RotatingWriter::open(&path, 32, 2).expect("відкриття");
        for _ in 0..12 {
            write_line(&writer, 30);
        }
        assert!(dir.join("core.1.log").exists());
        assert!(dir.join("core.2.log").exists());
        assert!(
            !dir.join("core.3.log").exists(),
            "понад max_backups файли не накопичуються"
        );
    }

    #[test]
    fn survives_missing_backups_chain() {
        // Ротація з «дірками» у ланцюжку бекапів не має падати.
        let dir = temp_dir("holes");
        let path = dir.join("core.log");
        let writer = RotatingWriter::open(&path, 16, 3).expect("відкриття");
        write_line(&writer, 15);
        write_line(&writer, 15); // ротація без наявних бекапів
        fs::remove_file(dir.join("core.1.log")).expect("видалення бекапу");
        write_line(&writer, 15); // ротація з відсутнім core.1.log
        assert!(path.exists());
    }
}
