use crate::database::{DatabaseManager, FileRecord};
use anyhow::Result;
use blake3::Hasher;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use walkdir::WalkDir;

pub struct VdfsIndexer;

impl VdfsIndexer {
    /// Berechnet den BLAKE3-Hash einer einzelnen Datei in 64KB-Chunks
    pub fn hash_file<P: AsRef<Path>>(path: P) -> Result<String> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut hasher = Hasher::new();
        let mut buffer = [0u8; 65536];

        loop {
            let bytes_read = reader.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        Ok(hasher.finalize().to_hex().to_string())
    }

    /// Scannt ein lokales Verzeichnis rekursiv und trägt alle Dateien ins Ledger ein
    pub fn scan_directory<P: AsRef<Path>>(
        db: &DatabaseManager,
        dir_path: P,
        device_id: &str,
    ) -> Result<usize> {
        let mut indexed_count = 0;
        let target_dir = dir_path.as_ref();

        if !target_dir.exists() {
            println!("⚠️ Warnung: Das Verzeichnis {} existiert nicht.", target_dir.display());
            return Ok(0);
        }

        println!("📂 Starte Scan für Verzeichnis: {}", target_dir.display());

        for entry in WalkDir::new(target_dir).follow_links(true).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            
            // Wir wollen nur echte Dateien indexieren, keine Ordner
            if path.is_file() {
                let file_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(name) => name.to_string(),
                    None => continue,
                };

                // Dateigröße ermitteln
                let metadata = match std::fs::metadata(path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let file_size = metadata.len() as i64;

                // BLAKE3-Hash berechnen
                let abs_path_str = path.to_string_lossy().to_string();
                match Self::hash_file(path) {
                    Ok(hash) => {
let file_record = FileRecord {
    blake3_hash: hash.clone(),
    file_name,
    file_size: file_size as u64, // oder: file_size.try_into().unwrap()
    mime_type: None,
    ticket: "".to_string(),
};
                        // In SQLite-Datenbank speichern
                        if let Err(e) = db.add_file(&file_record, device_id, &abs_path_str) {
                            eprintln!("❌ Fehler beim Speichern von {}: {}", file_record.file_name, e);
                        } else {
                            indexed_count += 1;
                        }
                    }
                    Err(e) => {
                        eprintln!("⚠️ Konnte Datei nicht hashen ({}): {}", abs_path_str, e);
                    }
                }
            }
        }

        println!("✅ Scan abgeschlossen! {} Dateien erfolgreich indiziert.", indexed_count);
        Ok(indexed_count)
    }
}
