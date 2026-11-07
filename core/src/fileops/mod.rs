//! Dateioperationen (Explorer): Papierkorb, kopieren, verschieben, umbenennen, Ordner anlegen.
//! `ops` und `trash` sind reine Dateisystem-Logik; Aufträge, „Rückgängig“ und Rechte liegen
//! in `jobs` bzw. der API-Schicht.

pub mod archive;
pub mod jobs;
pub mod ops;
pub mod trash;
