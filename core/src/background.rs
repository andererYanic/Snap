//! Hintergrundarbeit gegenüber der Oberfläche zurückstellen.
//!
//! Am Handy laufen Kern und Oberfläche im selben Prozess. Hasht der Kern zum Beispiel nach
//! „Alle Dateien“ den ganzen Speicher, konkurriert er mit der Oberfläche um CPU und Speicher-
//! zugriffe. [`lower_current_thread`] gibt dem aufrufenden Thread eine niedrigere CPU-Priorität
//! (nice 10, wie Androids `THREAD_PRIORITY_BACKGROUND`) und die niedrigste E/A-Stufe der
//! Klasse „best effort“. Er bekommt weiter Rechenzeit, sobald die Oberfläche nichts braucht –
//! es geht nichts verloren, die Arbeit dauert unter Last nur länger.

/// CPU-Priorität für Hintergrund-Threads (wie `android.os.Process.THREAD_PRIORITY_BACKGROUND`).
pub const NICE: i32 = 10;

/// Stellt den aufrufenden Thread zurück (Linux/Android; sonst ohne Wirkung). Fehler werden
/// ignoriert: Ohne die Absenkung läuft alles wie bisher, nur weniger rücksichtsvoll.
pub fn lower_current_thread() {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: gettid hat keine Vorbedingungen; setpriority/ioprio_set wirken nur auf den
        // eigenen Thread (unter Linux ist PRIO_PROCESS mit einer Thread-ID threadgenau).
        unsafe {
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            libc::setpriority(libc::PRIO_PROCESS, tid, NICE);
            libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, tid as libc::c_int, ioprio_best_effort(7));
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const IOPRIO_WHO_PROCESS: libc::c_int = 1;

/// E/A-Priorität „best effort“ (Klasse 2) mit Stufe 0 (hoch) bis 7 (niedrig).
#[cfg(any(target_os = "linux", target_os = "android"))]
const fn ioprio_best_effort(level: libc::c_int) -> libc::c_int {
    (2 << 13) | level
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod tests {
    use super::*;

    #[test]
    fn nur_der_eigene_thread_wird_zurueckgestellt() {
        // SAFETY: getpriority liest nur die Priorität des eigenen Threads.
        let before = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        let (nice, io) = std::thread::spawn(|| {
            lower_current_thread();
            // SAFETY: nur lesende Abfragen für den eigenen Thread.
            unsafe {
                let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
                let nice = libc::getpriority(libc::PRIO_PROCESS, tid);
                let io = libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, tid as libc::c_int);
                (nice, io)
            }
        })
        .join()
        .unwrap();
        assert_eq!(nice, before.max(NICE), "CPU-Priorität gesenkt");
        assert_eq!(io as libc::c_int, ioprio_best_effort(7), "E/A-Priorität gesenkt");
        // Der aufrufende Thread (hier: der Test) bleibt unverändert
        // SAFETY: getpriority liest nur die Priorität des eigenen Threads.
        let after = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        assert_eq!(after, before);
    }
}
