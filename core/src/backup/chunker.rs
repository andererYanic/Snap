//! Inhaltsbasierte Aufteilung in Stücke (FastCDC mit Normalisierung).
//!
//! Schnittpunkte hängen nur vom Inhalt ab: Wird in einer großen Datei etwas eingefügt, bleiben
//! die meisten Stücke gleich und müssen nicht erneut gespeichert werden (Deduplizierung).

use std::io::Read;

pub const MIN: usize = 256 * 1024;
pub const AVG: usize = 1024 * 1024;
pub const MAX: usize = 4 * 1024 * 1024;

// Masken in den oberen Bits (dort sammelt der Gear-Hash die meiste Historie).
const MASK_S: u64 = ((1u64 << 22) - 1) << (64 - 22); // vor AVG: seltener schneiden
const MASK_L: u64 = ((1u64 << 18) - 1) << (64 - 18); // nach AVG: häufiger schneiden

/// Feste Gear-Tabelle (aus einem festen Startwert erzeugt – Teil des Formats v1).
fn gear() -> &'static [u64; 256] {
    static TABLE: std::sync::OnceLock<[u64; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u64; 256];
        let mut x: u64 = 0x5eed_5a49_6b75_7031; // „snap-backup“
        for v in t.iter_mut() {
            // splitmix64
            x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            *v = z ^ (z >> 31);
        }
        t
    })
}

/// Länge des nächsten Stücks am Anfang von `data` (höchstens `MAX`).
pub fn cut_point(data: &[u8]) -> usize {
    let len = data.len();
    if len <= MIN {
        return len;
    }
    let g = gear();
    let mut hash: u64 = 0;
    let normal = AVG.min(len);
    let end = MAX.min(len);
    let mut i = MIN;
    while i < normal {
        hash = (hash << 1).wrapping_add(g[data[i] as usize]);
        if hash & MASK_S == 0 {
            return i + 1;
        }
        i += 1;
    }
    while i < end {
        hash = (hash << 1).wrapping_add(g[data[i] as usize]);
        if hash & MASK_L == 0 {
            return i + 1;
        }
        i += 1;
    }
    end
}

/// Zerlegt einen Datenstrom in Stücke.
pub struct Chunker<R> {
    reader: R,
    buf: Vec<u8>,
    eof: bool,
}

impl<R: Read> Chunker<R> {
    pub fn new(reader: R) -> Self {
        Self { reader, buf: Vec::with_capacity(MAX), eof: false }
    }

    fn fill(&mut self) -> std::io::Result<()> {
        while !self.eof && self.buf.len() < MAX {
            let start = self.buf.len();
            self.buf.resize(MAX, 0);
            match self.reader.read(&mut self.buf[start..]) {
                Ok(0) => {
                    self.buf.truncate(start);
                    self.eof = true;
                }
                Ok(n) => self.buf.truncate(start + n),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => self.buf.truncate(start),
                Err(e) => {
                    self.buf.truncate(start);
                    return Err(e);
                }
            }
        }
        Ok(())
    }
}

impl<R: Read> Iterator for Chunker<R> {
    type Item = std::io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Err(e) = self.fill() {
            return Some(Err(e));
        }
        if self.buf.is_empty() {
            return None;
        }
        let n = cut_point(&self.buf);
        let rest = self.buf.split_off(n);
        let chunk = std::mem::replace(&mut self.buf, rest);
        Some(Ok(chunk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    fn chunks(d: &[u8]) -> Vec<Vec<u8>> {
        Chunker::new(d).collect::<std::io::Result<Vec<_>>>().unwrap()
    }

    #[test]
    fn covers_input_with_bounded_sizes() {
        for len in [0, 1, MIN, MIN + 1, AVG, 3 * MAX + 17, 20 * 1024 * 1024] {
            let d = data(len, 7);
            let c = chunks(&d);
            assert_eq!(c.concat(), d, "{len}");
            for (i, ch) in c.iter().enumerate() {
                assert!(ch.len() <= MAX);
                if i + 1 < c.len() {
                    assert!(ch.len() > MIN, "nur das letzte Stück darf klein sein");
                }
            }
        }
        // Durchschnitt in der Nähe von AVG
        let d = data(64 * 1024 * 1024, 3);
        let n = chunks(&d).len();
        let avg = d.len() / n;
        assert!((AVG / 2..AVG * 2).contains(&avg), "Durchschnitt {avg}");
    }

    #[test]
    fn insertion_keeps_most_chunks() {
        let d = data(32 * 1024 * 1024, 11);
        let mut shifted = d.clone();
        shifted.splice(5_000_000..5_000_000, b"eingefuegt".iter().copied());
        let a: std::collections::HashSet<Vec<u8>> = chunks(&d).into_iter().collect();
        let b = chunks(&shifted);
        let same = b.iter().filter(|c| a.contains(*c)).count();
        assert!(same * 100 / b.len() >= 85, "{same} von {} Stücken gleich", b.len());
    }
}
