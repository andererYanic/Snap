//! Maschinenlesbare Fehlercodes für alle Oberflächen.
//!
//! Operationen geben `anyhow::Result` zurück. Wo ein Fehler eine bestimmte Bedeutung hat,
//! wird ein [`ApiError`] mit Code erzeugt (Makro [`api_bail!`]); er bleibt auch durch
//! `.context(...)` hindurch erkennbar. Alles andere wird als `Internal` gemeldet.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Ungültige Eingabe (Pfad, Hash, Code, Parameter).
    InvalidInput,
    /// Gesuchtes Objekt existiert nicht.
    NotFound,
    /// Eingabe passt auf mehrere Objekte.
    Ambiguous,
    /// Ziel existiert bereits (z. B. Datei ohne `force`).
    AlreadyExists,
    /// Gegenstelle nicht erreichbar.
    Unavailable,
    /// Gegenstelle hat abgelehnt (nicht freigegeben, Kopplung abgelehnt).
    Rejected,
    /// Keine Berechtigung.
    PermissionDenied,
    /// CLI/App und Daemon sprechen verschiedene API-Versionen.
    VersionMismatch,
    /// Zeitlimit abgelaufen.
    Timeout,
    /// Unerwarteter interner Fehler.
    Internal,
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// Bricht mit einem [`ApiError`] ab: `api_bail!(NotFound, "Datei {name} fehlt")`.
#[macro_export]
macro_rules! api_bail {
    ($code:ident, $($arg:tt)*) => {
        return Err($crate::api::error::ApiError::new($crate::api::error::ErrorCode::$code, format!($($arg)*)).into())
    };
}

/// Ermittelt den Code eines Fehlers (sucht auch durch `.context()`-Ketten).
pub fn code_of(e: &anyhow::Error) -> ErrorCode {
    e.chain().find_map(|c| c.downcast_ref::<ApiError>()).map(|a| a.code).unwrap_or(ErrorCode::Internal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    fn fails() -> anyhow::Result<()> {
        api_bail!(NotFound, "weg: {}", 42)
    }

    #[test]
    fn code_survives_context() {
        let e = fails().context("äußere Ebene").unwrap_err();
        assert_eq!(code_of(&e), ErrorCode::NotFound);
        assert!(format!("{e:#}").contains("weg: 42"));
        assert_eq!(code_of(&anyhow::anyhow!("x")), ErrorCode::Internal);
    }
}
