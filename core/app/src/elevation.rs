//! Elevation: запит адмін-прав з поясненням і шляхом відмови (T-034).
//!
//! architecture.md §2.1: MFT потребує elevation; стратегія — просити
//! сесійно з поясненням вигоди; за відмови — падати на обхід каталогів
//! (T-028) **без повторних запитів у тій самій сесії**.
//!
//! Чиста політика без I/O: shell тримає прапорець «відхилено в сесії»,
//! platform-win виконує UAC-relaunch; вибір MFT↔walk лишається за T-028.

/// Стан пропозиції elevation для UI (T-034).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationPromptKind {
    /// Уже з адмін-правами → MFT доступний (T-028).
    AlreadyElevated,
    /// Немає NTFS-томів, які б виграли від MFT → пропозиція зайва.
    NotNeeded,
    /// Є NTFS без elevation і користувач ще не відмовився → показати пояснення.
    Offer,
    /// Користувач відмовився в цій сесії → walk, без повторного запиту.
    DeclinedThisSession,
}

impl ElevationPromptKind {
    /// Стабільний рядок для health/IPC.
    pub fn as_str(self) -> &'static str {
        match self {
            ElevationPromptKind::AlreadyElevated => "elevated",
            ElevationPromptKind::NotNeeded => "not_needed",
            ElevationPromptKind::Offer => "offer",
            ElevationPromptKind::DeclinedThisSession => "declined",
        }
    }

    /// Чи UI має показувати банер/діалог з поясненням (активна пропозиція).
    pub fn offer_pending(self) -> bool {
        matches!(self, ElevationPromptKind::Offer)
    }
}

/// Сесійний стан відмови від elevation (один процес = одна сесія).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ElevationSession {
    declined: bool,
}

impl ElevationSession {
    pub fn new() -> Self {
        Self { declined: false }
    }

    pub fn is_declined(self) -> bool {
        self.declined
    }

    /// Зафіксувати відмову: подальші `evaluate` не пропонують знову (DoD T-034).
    pub fn decline(&mut self) {
        self.declined = true;
    }
}

/// Оцінити, чи показувати пропозицію elevation (T-034).
///
/// | elevated | declined | has_ntfs | результат |
/// |----------|----------|----------|-----------|
/// | true     | *        | *        | AlreadyElevated |
/// | false    | true     | *        | DeclinedThisSession |
/// | false    | false    | false    | NotNeeded |
/// | false    | false    | true     | Offer |
pub fn evaluate_elevation_prompt(
    is_elevated: bool,
    has_ntfs_volumes: bool,
    session: ElevationSession,
) -> ElevationPromptKind {
    if is_elevated {
        return ElevationPromptKind::AlreadyElevated;
    }
    if session.is_declined() {
        return ElevationPromptKind::DeclinedThisSession;
    }
    if has_ntfs_volumes {
        ElevationPromptKind::Offer
    } else {
        ElevationPromptKind::NotNeeded
    }
}

/// Пояснення вигоди elevation для UI (українською, готове до показу).
///
/// architecture.md §2.1: «просити elevation … з поясненням вигоди».
pub fn elevation_benefit_message() -> &'static str {
    "Адмін-права дозволяють швидке сканування NTFS (читання MFT) — \
     цифра «скільки можна звільнити» з’являється за секунди. \
     Без прав програма працює через обхід каталогів: повільніше, \
     але так само безпечно. Відмову можна змінити лише перезапуском."
}

/// Короткий підсумок для health / компактних UI.
pub fn elevation_benefit_summary() -> &'static str {
    "NTFS + admin → MFT (секунди); без прав → обхід каталогів"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevated_never_offers() {
        let s = ElevationSession::new();
        assert_eq!(
            evaluate_elevation_prompt(true, true, s),
            ElevationPromptKind::AlreadyElevated
        );
        // Навіть якщо «відмовились» до relaunch — elevated переважає.
        let mut declined = ElevationSession::new();
        declined.decline();
        assert_eq!(
            evaluate_elevation_prompt(true, true, declined),
            ElevationPromptKind::AlreadyElevated
        );
    }

    #[test]
    fn ntfs_without_elevation_offers_once() {
        let s = ElevationSession::new();
        let kind = evaluate_elevation_prompt(false, true, s);
        assert_eq!(kind, ElevationPromptKind::Offer);
        assert!(kind.offer_pending());
        assert_eq!(kind.as_str(), "offer");
    }

    #[test]
    fn decline_blocks_reprompt_in_session() {
        let mut s = ElevationSession::new();
        assert_eq!(
            evaluate_elevation_prompt(false, true, s),
            ElevationPromptKind::Offer
        );
        s.decline();
        let kind = evaluate_elevation_prompt(false, true, s);
        assert_eq!(kind, ElevationPromptKind::DeclinedThisSession);
        assert!(!kind.offer_pending());
        // Повторний decline ідемпотентний.
        s.decline();
        assert_eq!(
            evaluate_elevation_prompt(false, true, s),
            ElevationPromptKind::DeclinedThisSession
        );
    }

    #[test]
    fn no_ntfs_does_not_offer() {
        let s = ElevationSession::new();
        assert_eq!(
            evaluate_elevation_prompt(false, false, s),
            ElevationPromptKind::NotNeeded
        );
        // Після decline без NTFS — все одно declined (чесний сесійний стан).
        let mut d = ElevationSession::new();
        d.decline();
        assert_eq!(
            evaluate_elevation_prompt(false, false, d),
            ElevationPromptKind::DeclinedThisSession
        );
    }

    #[test]
    fn messages_are_non_empty_ukrainian_sentences() {
        let msg = elevation_benefit_message();
        assert!(msg.contains("MFT") || msg.contains("адмін") || msg.contains("Адмін"));
        assert!(msg.ends_with('.'));
        assert!(!elevation_benefit_summary().is_empty());
    }

    /// DoD T-034: з правами — MFT; після відмови — walk без re-prompt.
    /// (Вибір стратегії — T-028; тут — інваріант сесії elevation.)
    #[test]
    fn dod_with_rights_mft_path_available_after_decline_walk_no_reprompt() {
        use crate::scan_strategy::{choose_scan_strategy, VolumeCapabilities};

        // З правами → MFT.
        let mft = choose_scan_strategy(&VolumeCapabilities {
            is_ntfs: true,
            is_elevated: true,
        });
        assert_eq!(mft.strategy, trashradar_domain::scan::ScanStrategy::Mft);

        // Без прав після decline → walk + no offer.
        let mut session = ElevationSession::new();
        session.decline();
        let walk = choose_scan_strategy(&VolumeCapabilities {
            is_ntfs: true,
            is_elevated: false,
        });
        assert_eq!(
            walk.strategy,
            trashradar_domain::scan::ScanStrategy::DirectoryWalk
        );
        assert_eq!(
            evaluate_elevation_prompt(false, true, session),
            ElevationPromptKind::DeclinedThisSession
        );
        assert!(!evaluate_elevation_prompt(false, true, session).offer_pending());
    }
}
