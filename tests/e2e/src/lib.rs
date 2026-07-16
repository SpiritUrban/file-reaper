//! Харнес наскрізних тестів аварійного відновлення Quarantine (T-156).
//!
//! Що тут: спільний для батьківського тесту і дочірнього процесу-«жертви»
//! опис сценарію (розкладка каталогів, корпус файлів, ідентифікатори записів),
//! обгортки портів, що вбивають процес у заданій фазі операції, і перевірка
//! інваріантів безпеки після «рестарту».
//!
//! **Модель аварії.** Дочірній процес (`crash-victim`) виконує продуктовий
//! шлях reap/purge на **реальній** FS (`NativeQuarantineFs`) з **реальним**
//! SQLite-manifest і в заданій точці помирає через `std::process::exit` —
//! без розкрутки стека, без деструкторів, без коректного закриття БД. Далі
//! батьківський процес відкриває ту саму БД наново (це і є «наступний старт
//! застосунку», T-084) і звіряє журнал з реальністю через
//! `QuarantineRecovery::reconcile`.
//!
//! Чим це доповнює наявні тести: T-079/T-084 вправляють ті самі фази на
//! фейкових портах в одному процесі (швидко, у CI кожного крейта) — тут
//! перевіряється те, чого фейки не бачать: durability SQLite після
//! некоректного завершення процесу, реальні атомарні move і реальні файли
//! на диску.

use std::path::{Path, PathBuf};

use trashradar_app::ports::{
    QuarantineFs, QuarantineManifest, RecoveryLocation, RestoreMove, SurrogateState,
};
use trashradar_domain::error::CoreError;
use trashradar_domain::quarantine::{
    DestructiveAuditEvent, DestructiveAuditRecord, FileIdentity, QuarantineEntry,
    QuarantineEntryId, QuarantineStatus,
};

/// Скільки файлів у корпусі одного сценарію.
pub const CANDIDATE_FILES: u64 = 8;
/// Розмір файла: move не копіює дані, але ненульовий вміст робить перевірку
/// ідентичності (T-086) чесною.
pub const FILE_BYTES: usize = 2_048;
/// Код виходу «жертви»: процес загинув саме в точці ін'єкції, а не завершився.
pub const CRASH_EXIT_CODE: i32 = 42;
/// На якій за ліком операції падають фази «посеред батчу» (1-based).
pub const MID_BATCH_CALL: usize = 3;

/// Точка переривання. Матриця T-156 — це перелік цих значень.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPhase {
    /// Без аварії: операція виконується до кінця (сетап і контрольний рядок).
    None,
    /// Аварія до запису журналу: жодного сліду в manifest.
    ReapBeforeJournal,
    /// Аварія одразу після durable-запису всіх `in_flight`: move не стартував.
    ReapAfterJournal,
    /// Аварія посеред move'ів: частина файлів у карантині, решта на місці.
    ReapMidMove,
    /// Аварія після всіх move, до підтвердження: файли в карантині, журнал
    /// каже `in_flight`.
    ReapAfterMoves,
    /// Аварія одразу після підтвердження: стан уже консистентний.
    ReapAfterConfirm,
    /// Аварія до першого видалення: карантин недоторканий.
    PurgeBeforeDelete,
    /// Аварія між фізичним видаленням файла і оновленням статусу запису.
    PurgeAfterDelete,
    /// Аварія посеред батчу: кілька записів повністю оброблені, решта — ні.
    PurgeMidBatch,
}

impl CrashPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            CrashPhase::None => "none",
            CrashPhase::ReapBeforeJournal => "reap_before_journal",
            CrashPhase::ReapAfterJournal => "reap_after_journal",
            CrashPhase::ReapMidMove => "reap_mid_move",
            CrashPhase::ReapAfterMoves => "reap_after_moves",
            CrashPhase::ReapAfterConfirm => "reap_after_confirm",
            CrashPhase::PurgeBeforeDelete => "purge_before_delete",
            CrashPhase::PurgeAfterDelete => "purge_after_delete",
            CrashPhase::PurgeMidBatch => "purge_mid_batch",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            CrashPhase::None,
            CrashPhase::ReapBeforeJournal,
            CrashPhase::ReapAfterJournal,
            CrashPhase::ReapMidMove,
            CrashPhase::ReapAfterMoves,
            CrashPhase::ReapAfterConfirm,
            CrashPhase::PurgeBeforeDelete,
            CrashPhase::PurgeAfterDelete,
            CrashPhase::PurgeMidBatch,
        ]
        .into_iter()
        .find(|phase| phase.as_str() == value)
    }
}

/// Операція, яку виконує «жертва».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Reap,
    Purge,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Operation::Reap => "reap",
            Operation::Purge => "purge",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "reap" => Some(Operation::Reap),
            "purge" => Some(Operation::Purge),
            _ => None,
        }
    }
}

/// Розкладка сценарію: «том» (temp-каталог), дані користувача, профіль з БД.
/// Обидва процеси виводять усі шляхи звідси — жодного обміну станом, крім
/// кореня сценарію.
#[derive(Debug, Clone)]
pub struct Layout {
    pub root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Каталог «файлів користувача».
    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    /// Профіль застосунку з manifest-БД.
    pub fn profile_dir(&self) -> PathBuf {
        self.root.join("profile")
    }

    /// Каталог карантину тому (`<том>\.trashradar\quarantine`).
    pub fn quarantine_root(&self) -> PathBuf {
        self.root
            .join(trashradar_quarantine_fs::SERVICE_DIRECTORY_NAME)
            .join(trashradar_quarantine_fs::QUARANTINE_DIRECTORY_NAME)
    }

    /// Оригінальний шлях k-го кандидата.
    pub fn original_path(&self, index: u64) -> PathBuf {
        self.data_dir().join(format!("candidate{index}.bin"))
    }

    /// Сурогат k-го кандидата в карантині.
    pub fn surrogate_path(&self, index: u64) -> PathBuf {
        self.quarantine_root().join(surrogate_name(index))
    }
}

/// ID запису журналу для k-го кандидата (1-based — 0 лишається «порожнім»).
pub fn entry_id(index: u64) -> QuarantineEntryId {
    QuarantineEntryId(index + 1)
}

/// Сурогатне ім'я — той самий формат, що будує shell на реальному reap (T-138).
pub fn surrogate_name(index: u64) -> String {
    format!("{:016}.bin", entry_id(index).0)
}

/// Створити корпус кандидатів на диску.
pub fn create_candidates(layout: &Layout) -> std::io::Result<()> {
    std::fs::create_dir_all(layout.data_dir())?;
    let payload = vec![0xA5u8; FILE_BYTES];
    for index in 0..CANDIDATE_FILES {
        std::fs::write(layout.original_path(index), &payload)?;
    }
    Ok(())
}

/// Де фізично лежить кожен кандидат після «рестарту».
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    /// Файл на оригінальному місці (reap не відбувся або відкочений).
    pub original_only: Vec<u64>,
    /// Файл у карантині (reap відбувся).
    pub quarantine_only: Vec<u64>,
    /// **Дві копії** — дублювання даних, порушення D4.
    pub both: Vec<u64>,
    /// **Жодної копії** — втрата даних, порушення D4.
    pub missing: Vec<u64>,
}

/// Зняти інвентаризацію реальної FS (не журналу — саме диска).
pub fn inventory(layout: &Layout) -> Inventory {
    let mut inventory = Inventory::default();
    for index in 0..CANDIDATE_FILES {
        let at_source = layout.original_path(index).exists();
        let in_quarantine = layout.surrogate_path(index).exists();
        match (at_source, in_quarantine) {
            (true, false) => inventory.original_only.push(index),
            (false, true) => inventory.quarantine_only.push(index),
            (true, true) => inventory.both.push(index),
            (false, false) => inventory.missing.push(index),
        }
    }
    inventory
}

/// Головний інваріант D4: у кожного кандидата рівно одна копія — або на
/// оригінальному місці, або в карантині. Ні втрати, ні дублювання.
///
/// Єдиний дозволений виняток «жодної копії» — запис, який журнал свідомо
/// позначив `Purged`: файл знищено на вимогу користувача, і це не втрата.
pub fn assert_no_loss_no_duplicates(
    layout: &Layout,
    entries: &[QuarantineEntry],
    context: &str,
) -> Inventory {
    let inventory = inventory(layout);
    assert!(
        inventory.both.is_empty(),
        "{context}: дублювання — кандидати {:?} існують і на місці, і в карантині",
        inventory.both
    );
    let unexplained: Vec<u64> = inventory
        .missing
        .iter()
        .copied()
        .filter(|index| {
            !entries.iter().any(|entry| {
                entry.id == entry_id(*index) && entry.status == QuarantineStatus::Purged
            })
        })
        .collect();
    assert!(
        unexplained.is_empty(),
        "{context}: втрата даних — кандидати {unexplained:?} зникли з диска, і журнал не пояснює це знищенням"
    );
    inventory
}

/// Після відновлення журнал не має лишати незавершених транзакцій.
pub fn assert_no_in_flight(entries: &[QuarantineEntry], context: &str) {
    let in_flight: Vec<u64> = entries
        .iter()
        .filter(|entry| entry.status == QuarantineStatus::InFlight)
        .map(|entry| entry.id.0)
        .collect();
    assert!(
        in_flight.is_empty(),
        "{context}: після відновлення лишились незавершені записи {in_flight:?}"
    );
}

/// Жодного файла-сироти: у карантині немає файлів без запису журналу
/// (інакше вони б займали місце вічно, невидимі для UI і sweeper-а).
pub fn assert_no_orphan_files(layout: &Layout, entries: &[QuarantineEntry], context: &str) {
    let known: Vec<String> = entries
        .iter()
        .filter(|entry| entry.status == QuarantineStatus::Quarantined)
        .map(|entry| entry.surrogate_name.to_lowercase())
        .collect();
    let Ok(dir) = std::fs::read_dir(layout.quarantine_root()) else {
        return; // каталогу немає — сиріт бути не може
    };
    for item in dir.flatten() {
        let name = item.file_name().to_string_lossy().to_lowercase();
        assert!(
            known.contains(&name),
            "{context}: файл-сирота у карантині — «{name}» без запису журналу"
        );
    }
}

/// Дзеркальний інваріант: кожен «карантинований» запис журналу вказує на
/// реальний файл (інакше UI показує фантом, а restore нездійсненний).
pub fn assert_no_phantom_entries(layout: &Layout, entries: &[QuarantineEntry], context: &str) {
    let phantoms: Vec<u64> = entries
        .iter()
        .filter(|entry| entry.status == QuarantineStatus::Quarantined)
        .filter(|entry| {
            !layout
                .quarantine_root()
                .join(&entry.surrogate_name)
                .exists()
        })
        .map(|entry| entry.id.0)
        .collect();
    assert!(
        phantoms.is_empty(),
        "{context}: журнал обіцяє відновлення записів {phantoms:?}, але їхніх файлів немає на диску"
    );
}

/// План аварії для обгорток портів: у якій фазі і на якому за ліком виклику
/// процес має загинути.
#[derive(Debug, Clone, Copy)]
pub struct CrashPlan {
    pub phase: CrashPhase,
    /// 1-based номер виклику у своїй фазі (для «посеред батчу»).
    pub call: usize,
}

impl CrashPlan {
    pub fn new(phase: CrashPhase, call: usize) -> Self {
        Self { phase, call }
    }

    /// План для фази матриці: у якій точці ін'єкції та на якому за ліком
    /// виклику падати. Одні фази трапляються раз на батч (журнал,
    /// підтвердження), інші — на кожен файл (move, purge).
    pub fn for_phase(phase: CrashPhase) -> Self {
        match phase {
            // Одна операція на весь батч → перший же виклик.
            CrashPhase::ReapAfterJournal | CrashPhase::ReapAfterConfirm => Self::new(phase, 1),
            // Посеред батчу: частина зроблена, частина ні.
            CrashPhase::ReapMidMove | CrashPhase::PurgeAfterDelete | CrashPhase::PurgeMidBatch => {
                Self::new(phase, MID_BATCH_CALL)
            }
            // «Після всіх move, до підтвердження» — це аварія на останньому
            // move: точка ін'єкції та сама, змінюється лише номер виклику.
            CrashPhase::ReapAfterMoves => {
                Self::new(CrashPhase::ReapMidMove, CANDIDATE_FILES as usize)
            }
            // Фази без ін'єкції в портах: «жертва» гине сама, до виклику
            // use case (або не гине зовсім).
            CrashPhase::None | CrashPhase::ReapBeforeJournal | CrashPhase::PurgeBeforeDelete => {
                Self::new(phase, usize::MAX)
            }
        }
    }

    /// Померти, якщо це та сама фаза і той самий за ліком виклик.
    /// `exit` замість `panic` — це аварія процесу, а не помилка: жодних
    /// деструкторів, жодного коректного закриття SQLite.
    pub fn hit(&self, phase: CrashPhase, counter: &std::sync::atomic::AtomicUsize) {
        let call = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if self.phase == phase && call == self.call {
            eprintln!(
                "crash-victim: аварія у фазі {} (виклик {call})",
                phase.as_str()
            );
            std::process::exit(CRASH_EXIT_CODE);
        }
    }
}

/// Обгортка FS: делегує все реальному адаптеру, вбиваючи процес у заданій
/// фазі. Продуктовий шлях не змінюється — ін'єкція живе лише в тесті.
pub struct CrashingFs<F: QuarantineFs> {
    inner: F,
    plan: CrashPlan,
    moves: std::sync::atomic::AtomicUsize,
    purges: std::sync::atomic::AtomicUsize,
}

impl<F: QuarantineFs> CrashingFs<F> {
    pub fn new(inner: F, plan: CrashPlan) -> Self {
        Self {
            inner,
            plan,
            moves: std::sync::atomic::AtomicUsize::new(0),
            purges: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl<F: QuarantineFs> QuarantineFs for CrashingFs<F> {
    fn move_into_quarantine(
        &self,
        source: &str,
        destination: &str,
        expected: FileIdentity,
        roots: &[String],
    ) -> Result<(), CoreError> {
        let result = self
            .inner
            .move_into_quarantine(source, destination, expected, roots);
        // Після фактичного move: файл уже в карантині, журнал ще каже in_flight.
        self.plan.hit(CrashPhase::ReapMidMove, &self.moves);
        result
    }

    fn restore_from_quarantine(
        &self,
        surrogate_path: &str,
        destination_path: &str,
    ) -> Result<RestoreMove, CoreError> {
        self.inner
            .restore_from_quarantine(surrogate_path, destination_path)
    }

    fn purge_from_quarantine(&self, surrogate_path: &str) -> Result<(), CoreError> {
        let result = self.inner.purge_from_quarantine(surrogate_path);
        // Після фактичного видалення: файла вже немає, статус ще Quarantined.
        self.plan.hit(CrashPhase::PurgeAfterDelete, &self.purges);
        result
    }

    fn recovery_location(
        &self,
        source_path: &str,
        surrogate_path: &str,
    ) -> Result<RecoveryLocation, CoreError> {
        self.inner.recovery_location(source_path, surrogate_path)
    }

    fn surrogate_state(&self, surrogate_path: &str) -> Result<SurrogateState, CoreError> {
        self.inner.surrogate_state(surrogate_path)
    }
}

/// Обгортка manifest: ті самі ін'єкції на межах журнальних фаз.
/// Батч-методи ОБОВ'ЯЗКОВО делегуються — інакше тест ганяв би дефолтні
/// per-item цикли порту, а не реальні SQLite-транзакції (той самий нюанс,
/// що й у reap-bench).
pub struct CrashingManifest<M: QuarantineManifest> {
    inner: M,
    plan: CrashPlan,
    journals: std::sync::atomic::AtomicUsize,
    confirms: std::sync::atomic::AtomicUsize,
    confirmations: std::sync::atomic::AtomicUsize,
}

impl<M: QuarantineManifest> CrashingManifest<M> {
    pub fn new(inner: M, plan: CrashPlan) -> Self {
        Self {
            inner,
            plan,
            journals: std::sync::atomic::AtomicUsize::new(0),
            confirms: std::sync::atomic::AtomicUsize::new(0),
            confirmations: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl<M: QuarantineManifest> QuarantineManifest for CrashingManifest<M> {
    fn insert_entry(&self, entry: &QuarantineEntry) -> Result<(), CoreError> {
        self.inner.insert_entry(entry)
    }

    fn insert_entries(&self, entries: &[QuarantineEntry]) -> Result<(), CoreError> {
        let result = self.inner.insert_entries(entries);
        self.plan.hit(CrashPhase::ReapAfterJournal, &self.journals);
        result
    }

    fn get_entry(&self, id: QuarantineEntryId) -> Result<Option<QuarantineEntry>, CoreError> {
        self.inner.get_entry(id)
    }

    fn list_entries(&self) -> Result<Vec<QuarantineEntry>, CoreError> {
        self.inner.list_entries()
    }

    fn remove_entry(&self, id: QuarantineEntryId) -> Result<(), CoreError> {
        self.inner.remove_entry(id)
    }

    fn append_audit(&self, event: &DestructiveAuditEvent) -> Result<(), CoreError> {
        self.inner.append_audit(event)
    }

    fn list_audit(&self) -> Result<Vec<DestructiveAuditRecord>, CoreError> {
        self.inner.list_audit()
    }

    fn confirm_batch_with_audit(
        &self,
        confirmations: &[(QuarantineEntryId, QuarantineStatus, DestructiveAuditEvent)],
    ) -> Result<(), CoreError> {
        let result = self.inner.confirm_batch_with_audit(confirmations);
        self.plan.hit(CrashPhase::ReapAfterConfirm, &self.confirms);
        result
    }

    /// Шлях purge (`ManualPurger`/`QuarantineSweeper`): один запис = один
    /// перехід статусу + аудит. Делегувати обов'язково — у SQLite це одна
    /// транзакція, а дефолт порту розпав би її на update+append.
    fn confirm_with_audit(
        &self,
        id: QuarantineEntryId,
        status: QuarantineStatus,
        event: &DestructiveAuditEvent,
    ) -> Result<(), CoreError> {
        let result = self.inner.confirm_with_audit(id, status, event);
        // Після повної обробки запису: файла немає, статус уже Purged.
        self.plan
            .hit(CrashPhase::PurgeMidBatch, &self.confirmations);
        result
    }

    fn update_status(
        &self,
        id: QuarantineEntryId,
        status: QuarantineStatus,
    ) -> Result<(), CoreError> {
        self.inner.update_status(id, status)
    }
}

/// Прибрати каталог сценарію (між рядками матриці стан не переноситься).
pub fn reset_root(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}
