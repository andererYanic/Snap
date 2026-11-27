//! RAW-Fotos (Kamera-Rohdaten): eingebettete JPEG-Vorschau finden.
//!
//! Fast jede Kamera legt in ihre RAW-Datei eine fertige JPEG-Vorschau (oft in voller Größe).
//! Statt die Rohdaten zu entwickeln, zeigt Snap diese Vorschau – schnell, ohne Zusatzbibliothek.
//!
//! Unterstützt: TIFF-basierte Formate (CR2, NEF/NRW, ARW/SR2, DNG, ORF, RW2, PEF, SRW, 3FR, ERF,
//! KDC, MOS) und Fujifilm RAF.
//!
//! Die Datei kommt von außen (feindlich): Jeder Versatz wird gegen die Dateigröße geprüft, die
//! Zahl der IFDs, Einträge und die Verschachtelung sind begrenzt, Schleifen in der IFD-Kette werden
//! erkannt. Ergebnis ist nur ein Bereich der Datei, der mit einem JPEG-Kopf beginnt; decodiert
//! wird er wie jedes andere JPEG (mit dessen Grenzen).

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

/// Endungen, die als RAW gelten.
pub const EXTENSIONS: &[&str] =
    &["cr2", "nef", "nrw", "arw", "sr2", "srf", "dng", "orf", "rw2", "pef", "srw", "raf", "3fr", "erf", "kdc", "mos"];

/// Größte Vorschau, die übernommen wird.
pub const MAX_PREVIEW: u64 = 64 * 1024 * 1024;
const MAX_IFDS: usize = 64;
const MAX_ENTRIES: u16 = 1024;
const MAX_DEPTH: u8 = 4;

/// Ist `name` eine RAW-Datei (nach Endung)?
pub fn is_raw(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, e)| EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Gefundene Vorschau: Bereich in der Datei und Ausrichtung (EXIF 1–8) aus IFD0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preview {
    pub offset: u64,
    pub len: u64,
    pub orientation: u16,
}

#[derive(Clone, Copy)]
enum Order {
    Le,
    Be,
}

struct Tiff<'a, R> {
    r: &'a mut R,
    order: Order,
    /// Basis der Versätze (bei RAF/RW2-Varianten nicht 0).
    base: u64,
    size: u64,
}

impl<R: Read + Seek> Tiff<'_, R> {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Option<()> {
        let abs = self.base.checked_add(off)?;
        if abs.checked_add(buf.len() as u64)? > self.size {
            return None;
        }
        self.r.seek(SeekFrom::Start(abs)).ok()?;
        self.r.read_exact(buf).ok()
    }
    fn u16_at(&mut self, off: u64) -> Option<u16> {
        let mut b = [0u8; 2];
        self.read_at(off, &mut b)?;
        Some(match self.order {
            Order::Le => u16::from_le_bytes(b),
            Order::Be => u16::from_be_bytes(b),
        })
    }
    fn u32_at(&mut self, off: u64) -> Option<u32> {
        let mut b = [0u8; 4];
        self.read_at(off, &mut b)?;
        Some(match self.order {
            Order::Le => u32::from_le_bytes(b),
            Order::Be => u32::from_be_bytes(b),
        })
    }
    /// Wert eines Eintrags (SHORT oder LONG, Anzahl 1) bzw. der Versatz bei mehreren Werten.
    fn value(&mut self, entry: u64, typ: u16, count: u32) -> Option<u32> {
        match (typ, count) {
            (3, 1) => self.u16_at(entry + 8).map(u32::from),
            (4 | 13, 1) => self.u32_at(entry + 8),
            (4 | 13, _) => {
                let off = self.u32_at(entry + 8)?;
                self.u32_at(u64::from(off))
            }
            _ => None,
        }
    }
    /// Erste Werte einer Liste von LONGs/SHORTs (für SubIFDs).
    fn values(&mut self, entry: u64, typ: u16, count: u32) -> Vec<u32> {
        let n = count.min(8);
        let width = if typ == 3 { 2 } else { 4 };
        let base = if u64::from(n) * width <= 4 {
            entry + 8
        } else {
            match self.u32_at(entry + 8) {
                Some(o) => u64::from(o),
                None => return Vec::new(),
            }
        };
        (0..u64::from(n))
            .filter_map(|i| if typ == 3 { self.u16_at(base + i * 2).map(u32::from) } else { self.u32_at(base + i * 4) })
            .collect()
    }

    /// Prüft, ob an `off` ein JPEG beginnt und ganz in der Datei liegt.
    fn is_jpeg(&mut self, off: u64, len: u64) -> bool {
        let mut b = [0u8; 3];
        (128..=MAX_PREVIEW).contains(&len)
            && self.base.checked_add(off).and_then(|a| a.checked_add(len)).is_some_and(|end| end <= self.size)
            && self.read_at(off, &mut b).is_some()
            && b == [0xFF, 0xD8, 0xFF]
    }
}

/// Sucht die größte eingebettete JPEG-Vorschau. `size`: Länge der Datei.
pub fn find_preview<R: Read + Seek>(r: &mut R, size: u64) -> Option<Preview> {
    let mut head = [0u8; 16];
    r.seek(SeekFrom::Start(0)).ok()?;
    let n = r.read(&mut head).ok()?;
    if n >= 16 && head.starts_with(b"FUJIFILMCCD-RAW") {
        return raf(r, size);
    }
    if n < 8 {
        return None;
    }
    let order = match &head[..2] {
        b"II" => Order::Le,
        b"MM" => Order::Be,
        _ => return None,
    };
    let mut t = Tiff { r, order, base: 0, size };
    // Magie: 42 (TIFF), 0x4F52/0x5352 (ORF), 0x55 (RW2)
    let magic = t.u16_at(2)?;
    if !matches!(magic, 42 | 0x4F52 | 0x5352 | 0x55) {
        return None;
    }
    let first = t.u32_at(4)?;
    let mut best: Option<(u64, u64)> = None;
    let mut orientation = 1u16;
    let mut seen = HashSet::new();
    let mut queue: Vec<(u64, u8, bool)> = vec![(u64::from(first), 0, true)];
    while let Some((ifd, depth, top)) = queue.pop() {
        if seen.len() >= MAX_IFDS || !seen.insert(ifd) || ifd == 0 {
            continue;
        }
        let Some(count) = t.u16_at(ifd) else { continue };
        if count == 0 || count > MAX_ENTRIES {
            continue;
        }
        let (mut jpeg_off, mut jpeg_len, mut strip_off, mut strip_len, mut compression) =
            (None, None, None, None, 0u32);
        for i in 0..u64::from(count) {
            let e = ifd + 2 + i * 12;
            let (Some(tag), Some(typ), Some(cnt)) = (t.u16_at(e), t.u16_at(e + 2), t.u32_at(e + 4)) else { break };
            match tag {
                0x0103 => compression = t.value(e, typ, cnt).unwrap_or(0),
                0x0111 if cnt == 1 => strip_off = t.value(e, typ, cnt),
                0x0117 if cnt == 1 => strip_len = t.value(e, typ, cnt),
                0x0112 if top && depth == 0 => {
                    if let Some(o) = t.value(e, typ, cnt).filter(|o| (1..=8).contains(o)) {
                        orientation = o as u16;
                    }
                }
                0x0201 => jpeg_off = t.value(e, typ, cnt),
                0x0202 => jpeg_len = t.value(e, typ, cnt),
                // RW2: ganzes eingebettetes JPEG als Datenblock
                0x002E if magic == 0x55 && typ == 7 => {
                    if let Some(off) = t.u32_at(e + 8) {
                        jpeg_off = Some(off);
                        jpeg_len = Some(cnt);
                    }
                }
                // SubIFDs (DNG, NEF, ARW) und EXIF-IFD
                0x014A | 0x8769 if depth < MAX_DEPTH => {
                    for sub in t.values(e, typ, cnt) {
                        queue.push((u64::from(sub), depth + 1, false));
                    }
                }
                _ => {}
            }
        }
        let mut consider = |off: Option<u32>, len: Option<u32>, t: &mut Tiff<'_, R>| {
            if let (Some(o), Some(l)) = (off, len) {
                let (o, l) = (u64::from(o), u64::from(l));
                if t.is_jpeg(o, l) && best.is_none_or(|(_, bl)| l > bl) {
                    best = Some((o, l));
                }
            }
        };
        consider(jpeg_off, jpeg_len, &mut t);
        if matches!(compression, 6 | 7) {
            consider(strip_off, strip_len, &mut t);
        }
        // Nächstes IFD der Kette (IFD0 → IFD1 …)
        if let Some(next) = t.u32_at(ifd + 2 + u64::from(count) * 12) {
            queue.push((u64::from(next), depth, top));
        }
    }
    best.map(|(offset, len)| Preview { offset, len, orientation })
}

/// Fujifilm RAF: Versatz und Länge der JPEG-Vorschau stehen fest an Byte 84/88 (big endian).
fn raf<R: Read + Seek>(r: &mut R, size: u64) -> Option<Preview> {
    let mut b = [0u8; 8];
    r.seek(SeekFrom::Start(84)).ok()?;
    r.read_exact(&mut b).ok()?;
    let off = u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let len = u64::from(u32::from_be_bytes([b[4], b[5], b[6], b[7]]));
    let mut t = Tiff { r, order: Order::Be, base: 0, size };
    t.is_jpeg(off, len).then_some(Preview { offset: off, len, orientation: 1 })
}

/// Vorschau als Bytes lesen (höchstens [`MAX_PREVIEW`]).
pub fn read_preview<R: Read + Seek>(r: &mut R, size: u64) -> Option<(Vec<u8>, u16)> {
    let p = find_preview(r, size)?;
    r.seek(SeekFrom::Start(p.offset)).ok()?;
    let mut buf = vec![0u8; usize::try_from(p.len).ok()?];
    r.read_exact(&mut buf).ok()?;
    Some((buf, p.orientation))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    /// Kleines TIFF (little endian): IFD0 mit Ausrichtung und SubIFD, darin die JPEG-Vorschau.
    pub(crate) fn fake_raw(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"II*\0");
        v.extend_from_slice(&8u32.to_le_bytes());
        // IFD0 bei 8: 2 Einträge
        let ifd0 = 8u32;
        let sub = ifd0 + 2 + 2 * 12 + 4;
        let data = sub + 2 + 2 * 12 + 4;
        v.extend_from_slice(&2u16.to_le_bytes());
        entry(&mut v, 0x0112, 3, 1, u32::from(orientation));
        entry(&mut v, 0x014A, 4, 1, sub);
        v.extend_from_slice(&0u32.to_le_bytes());
        // SubIFD: JPEGInterchangeFormat + Länge
        v.extend_from_slice(&2u16.to_le_bytes());
        entry(&mut v, 0x0201, 4, 1, data);
        entry(&mut v, 0x0202, 4, 1, jpeg.len() as u32);
        v.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(v.len() as u32, data);
        v.extend_from_slice(jpeg);
        v
    }

    fn entry(v: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32) {
        v.extend_from_slice(&tag.to_le_bytes());
        v.extend_from_slice(&typ.to_le_bytes());
        v.extend_from_slice(&count.to_le_bytes());
        if typ == 3 {
            v.extend_from_slice(&(value as u16).to_le_bytes());
            v.extend_from_slice(&[0, 0]);
        } else {
            v.extend_from_slice(&value.to_le_bytes());
        }
    }

    #[test]
    fn vorschau_aus_subifd() {
        let jpeg = crate::media::tests::jpeg(64, 48);
        let raw = fake_raw(&jpeg, 6);
        let (bytes, o) = read_preview(&mut Cursor::new(&raw), raw.len() as u64).expect("gefunden");
        assert_eq!(bytes, jpeg);
        assert_eq!(o, 6);
        // Daraus entsteht ein Vorschaubild (die Ausrichtung wendet der Aufrufer an)
        let img = crate::media::render_thumbnail(Cursor::new(bytes), 256).unwrap();
        assert_eq!((img.width(), img.height()), (64, 48));
    }

    #[test]
    fn kaputte_und_boesartige_dateien() {
        let jpeg = crate::media::tests::jpeg(64, 48);
        let raw = fake_raw(&jpeg, 1);
        // Abgeschnitten: Vorschau ragt über das Dateiende
        let cut = &raw[..raw.len() - 10];
        assert!(find_preview(&mut Cursor::new(cut), cut.len() as u64).is_none());
        // Schleife in der IFD-Kette (IFD0 zeigt auf sich selbst)
        let mut looped = raw.clone();
        let next = 8 + 2 + 2 * 12;
        looped[next..next + 4].copy_from_slice(&8u32.to_le_bytes());
        assert!(find_preview(&mut Cursor::new(&looped), looped.len() as u64).is_some());
        // Kein TIFF, kein JPEG, leere Datei
        for bad in [&b""[..], b"hallo welt", b"II*\0\xff\xff\xff\xff", b"MM\0*\0\0\0\x08\0\0"] {
            assert!(find_preview(&mut Cursor::new(bad), bad.len() as u64).is_none());
        }
        assert!(is_raw("IMG_0001.CR2") && is_raw("a.dng") && !is_raw("a.jpg") && !is_raw("raw"));
    }

    #[test]
    fn raf_vorschau() {
        let jpeg = crate::media::tests::jpeg(32, 32);
        let mut v = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        v.resize(84, 0);
        let off = 100u32;
        v.extend_from_slice(&off.to_be_bytes());
        v.extend_from_slice(&(jpeg.len() as u32).to_be_bytes());
        v.resize(off as usize, 0);
        v.extend_from_slice(&jpeg);
        let (bytes, _) = read_preview(&mut Cursor::new(&v), v.len() as u64).unwrap();
        assert_eq!(bytes, jpeg);
    }
}
