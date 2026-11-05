//! Exponentielles Backoff mit Jitter für nicht erreichbare Peers.
//!
//! Stufen: 30 s → 2 min → 10 min → 1 h (Maximum). Jitter ±20 %, damit
//! Wiederholungsversuche kein festes, wiedererkennbares Zeitmuster erzeugen.

use std::time::Duration;

const STEPS_SECS: [u64; 4] = [30, 120, 600, 3600];
const JITTER: f64 = 0.2;

/// Wartezeit nach `failures` aufeinanderfolgenden Fehlschlägen (>= 1).
/// `jitter` liegt in `[-1.0, 1.0]` und skaliert die ±20 % Streuung.
pub fn delay(failures: u32, jitter: f64) -> Duration {
    let idx = (failures.max(1) as usize - 1).min(STEPS_SECS.len() - 1);
    let base = STEPS_SECS[idx] as f64;
    let factor = 1.0 + JITTER * jitter.clamp(-1.0, 1.0);
    Duration::from_secs_f64((base * factor).max(1.0))
}

/// Wartezeit mit zufälligem Jitter.
pub fn delay_random(failures: u32) -> Duration {
    delay(failures, fastrand::f64() * 2.0 - 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_without_jitter() {
        assert_eq!(delay(1, 0.0).as_secs(), 30);
        assert_eq!(delay(2, 0.0).as_secs(), 120);
        assert_eq!(delay(3, 0.0).as_secs(), 600);
        assert_eq!(delay(4, 0.0).as_secs(), 3600);
        assert_eq!(delay(100, 0.0).as_secs(), 3600);
        assert_eq!(delay(0, 0.0).as_secs(), 30);
    }

    #[test]
    fn jitter_bounds() {
        assert_eq!(delay(1, -1.0).as_secs(), 24);
        assert_eq!(delay(1, 1.0).as_secs(), 36);
        assert_eq!(delay(4, 5.0).as_secs(), 4320);
        for _ in 0..1000 {
            let d = delay_random(4).as_secs();
            assert!((2880..=4320).contains(&d));
        }
    }
}
