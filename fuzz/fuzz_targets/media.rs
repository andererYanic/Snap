//! Bild-Decoder, EXIF und Audio-/Video-Tags mit beliebigen Bytes (Dateien fremder Herkunft).
//! Invarianten: kein Absturz, Vorschaubilder nie größer als die gewünschte Kante,
//! GPS-Koordinaten im gültigen Bereich, Dauer endlich und positiv, Texte ohne Steuerzeichen.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::media::{read_exif, read_tags, render_thumbnail_limited, MediaInfo};
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    // 16 MiB statt 256 MiB: gleiche Codepfade, aber schnelle Läufe.
    if let Ok(img) = render_thumbnail_limited(Cursor::new(data), 64, 16 * 1024 * 1024) {
        assert!(img.width() <= 64 && img.height() <= 64, "{}x{}", img.width(), img.height());
    }
    let mut info = MediaInfo::default();
    read_exif(&mut Cursor::new(data), &mut info);
    read_tags(&mut Cursor::new(data), &mut info);
    if let Some(g) = info.gps {
        assert!(g.lat.is_finite() && g.lat.abs() <= 90.0 && g.lon.is_finite() && g.lon.abs() <= 180.0);
    }
    if let Some(d) = info.duration_secs {
        assert!(d.is_finite() && d > 0.0);
    }
    if let Some(o) = info.orientation {
        assert!((1..=8).contains(&o));
    }
    for t in [&info.title, &info.artist, &info.album, &info.camera, &info.taken_at].into_iter().flatten() {
        assert!(!t.chars().any(char::is_control), "{t:?}");
    }
});
