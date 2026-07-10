//! Модель помилок Core (T-007).
//!
//! Контракт: будь-яка помилка, що перетинає межу IPC, — це [`CoreError`]
//! зі **стабільним кодом** (машиночитаний, snake_case, ніколи не
//! перейменовується) і **людським текстом** (показується користувачу
//! як є). UI розгалужується за `code`, ніколи за текстом.
//!
//! Конверт серіалізації: `{"code": "...", "message": "..."}` —
//! див. contracts/ipc-contract.json → error_envelope.
//!
//! Стартовий набір кодів покриває каркас; підсистеми додають власні
//! коди своїми задачами (guard-list → `path_protected` у T-085 тощо).
//! Додавати код — можна, змінювати чи видаляти наявний — заборонено.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Стабільні машиночитані коди помилок.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Непередбачена внутрішня помилка (баг або невідомий стан).
    Internal,
    /// Некоректний аргумент команди — помилка викликача, не Core.
    InvalidArgument,
    /// Команда оголошена в контракті, але ще не реалізована.
    NotImplemented,
    /// Помилка вводу/виводу (диск, файл, доступ).
    Io,
    /// Операцію скасовано (кооперативна відміна — architecture.md §14).
    Cancelled,
    /// Шлях захищений guard-list і не може бути переміщений у Quarantine.
    PathProtected,
}

impl ErrorCode {
    /// Стабільний рядковий код — той самий, що у серіалізованому конверті.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Internal => "internal",
            ErrorCode::InvalidArgument => "invalid_argument",
            ErrorCode::NotImplemented => "not_implemented",
            ErrorCode::Io => "io",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::PathProtected => "path_protected",
        }
    }
}

/// Помилка, що перетинає межу Core → UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreError {
    pub code: ErrorCode,
    /// Людський текст для користувача — повне речення, без жаргону.
    pub message: String,
}

impl CoreError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn not_implemented(command: &str) -> Self {
        Self::new(
            ErrorCode::NotImplemented,
            format!("Команда «{command}» ще не реалізована."),
        )
    }

    pub fn cancelled(operation: &str) -> Self {
        Self::new(
            ErrorCode::Cancelled,
            format!("Операцію «{operation}» скасовано."),
        )
    }

    pub fn path_protected(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PathProtected, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Io, message)
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for CoreError {}

/// I/O-помилки — найчастіше джерело у продукті про диски.
impl From<std::io::Error> for CoreError {
    fn from(err: std::io::Error) -> Self {
        Self::new(ErrorCode::Io, format!("Помилка доступу до диска: {err}."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_stable_envelope() {
        let error = CoreError::not_implemented("scan.start");
        let value = serde_json::to_value(&error).expect("серіалізація");
        assert_eq!(value["code"], "not_implemented");
        assert!(value["message"]
            .as_str()
            .expect("message — рядок")
            .contains("scan.start"));
        assert_eq!(
            value.as_object().expect("обʼєкт").len(),
            2,
            "конверт містить рівно code і message"
        );
    }

    #[test]
    fn roundtrips_through_serde() {
        let original = CoreError::invalid_argument("Порог має бути додатним.");
        let json = serde_json::to_string(&original).expect("серіалізація");
        let parsed: CoreError = serde_json::from_str(&json).expect("десеріалізація");
        assert_eq!(parsed, original);
    }

    #[test]
    fn io_error_converts_with_io_code() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let error: CoreError = io.into();
        assert_eq!(error.code, ErrorCode::Io);
        assert!(!error.message.is_empty());
    }

    #[test]
    fn string_codes_match_serde_representation() {
        for code in [
            ErrorCode::Internal,
            ErrorCode::InvalidArgument,
            ErrorCode::NotImplemented,
            ErrorCode::Io,
            ErrorCode::Cancelled,
            ErrorCode::PathProtected,
        ] {
            let serialized = serde_json::to_value(code).expect("серіалізація");
            assert_eq!(serialized, code.as_str(), "as_str і serde збігаються");
        }
    }
}
