//! Systemmitteilungen für Snap Drop (Linux, über `org.freedesktop.Notifications`), solange keine
//! Oberfläche verbunden ist: Ist die Desktop-App zu, erscheint ein Angebot trotzdem – mit
//! „Annehmen“/„Ablehnen“ direkt in der Mitteilung. Ohne Sitzungsbus (z. B. auf einem Server)
//! passiert einfach nichts.

use space_core::events::Event;
use space_core::ipc::Request;
use space_core::Daemon;
use std::collections::HashMap;
use tokio_stream::StreamExt;
use zbus::Variant as Value;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
}

/// Deutsch oder Englisch nach der Spracheinstellung der Sitzung.
fn german() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| v.starts_with("de"))
}

fn size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Text einer Mitteilung: nur druckbare Zeichen, kein Markup (manche Server werten `<b>` aus).
fn plain(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == '<' || c == '>' || c == '&' { ' ' } else { c })
        .take(max)
        .collect()
}

pub async fn run(daemon: Daemon) {
    if let Err(e) = serve(daemon).await {
        tracing::debug!("Keine Systemmitteilungen: {e:#}");
    }
}

async fn serve(daemon: Daemon) -> anyhow::Result<()> {
    let conn = zbus::Connection::session().await?;
    let proxy = NotificationsProxy::new(&conn).await?;
    let mut actions = proxy.receive_action_invoked().await?;
    let mut events = daemon.subscribe();
    let de = german();
    // Mitteilung → Angebot
    let mut shown: HashMap<u32, u64> = HashMap::new();
    let mut by_offer: HashMap<u64, u32> = HashMap::new();
    loop {
        tokio::select! {
            ev = events.next() => {
                let Some(ev) = ev else { return Ok(()) };
                match ev {
                    Event::DropOffer { id, name, contact, code, files, total, .. } if daemon.ui_clients() == 0 => {
                        let who = plain(contact.as_deref().unwrap_or(&name), 64);
                        let what = match (files, de) {
                            (0, true) => "möchte dir einen Text senden".to_string(),
                            (0, false) => "wants to send you a text".to_string(),
                            (1, true) => format!("möchte dir 1 Datei senden ({})", size(total)),
                            (1, false) => format!("wants to send you 1 file ({})", size(total)),
                            (n, true) => format!("möchte dir {n} Dateien senden ({})", size(total)),
                            (n, false) => format!("wants to send you {n} files ({})", size(total)),
                        };
                        let body = if contact.is_some() {
                            what
                        } else if de {
                            format!("{what}\nPrüfcode {code} – nur annehmen, wenn du die Person kennst.")
                        } else {
                            format!("{what}\nCheck code {code} – only accept if you know this person.")
                        };
                        let (yes, no) = if de { ("Annehmen", "Ablehnen") } else { ("Accept", "Decline") };
                        let mut hints = HashMap::new();
                        hints.insert("urgency", Value::from(2u8));
                        hints.insert("desktop-entry", Value::from("de.snap.desktop"));
                        match proxy.notify("Snap Drop", 0, "de.snap.desktop", &who, &plain(&body, 400), &["decline", no, "accept", yes], hints, 120_000).await {
                            Ok(nid) => {
                                shown.insert(nid, id);
                                by_offer.insert(id, nid);
                            }
                            Err(e) => tracing::debug!("Mitteilung nicht gezeigt: {e}"),
                        }
                    }
                    // Inhalt nie in der Mitteilung (kann ein Passwort sein)
                    Event::ClipboardReceived { name, .. } if daemon.ui_clients() == 0 => {
                        let body = if de {
                            "Zwischenablage empfangen – öffne Snap, dann liegt sie bereit."
                        } else {
                            "Clipboard received – open Snap and it's ready to paste."
                        };
                        let mut hints = HashMap::new();
                        hints.insert("desktop-entry", Value::from("de.snap.desktop"));
                        let _ = proxy.notify("Snap", 0, "de.snap.desktop", &plain(&name, 64), body, &[], hints, 10_000).await;
                    }
                    Event::DropOfferClosed { id, .. } => {
                        if let Some(nid) = by_offer.remove(&id) {
                            shown.remove(&nid);
                            let _ = proxy.close_notification(nid).await;
                        }
                    }
                    Event::DropFinished { id, incoming: true, ok, peer, files, text, .. } if daemon.ui_clients() == 0 || by_offer.contains_key(&id) => {
                        by_offer.remove(&id);
                        let body = match (ok, text.is_some(), de) {
                            (false, _, true) => "Empfang fehlgeschlagen.".to_string(),
                            (false, _, false) => "Receiving failed.".to_string(),
                            (true, true, true) => "Text empfangen – öffne Snap, um ihn zu kopieren.".to_string(),
                            (true, true, false) => "Text received – open Snap to copy it.".to_string(),
                            (true, false, true) => format!("{} Datei(en) gespeichert.", files.len()),
                            (true, false, false) => format!("{} file(s) saved.", files.len()),
                        };
                        let mut hints = HashMap::new();
                        hints.insert("desktop-entry", Value::from("de.snap.desktop"));
                        let _ = proxy.notify("Snap Drop", 0, "de.snap.desktop", &plain(&peer, 64), &body, &[], hints, 10_000).await;
                    }
                    _ => {}
                }
            }
            sig = actions.next() => {
                let Some(sig) = sig else { return Ok(()) };
                let Ok(args) = sig.args() else { continue };
                // Nur Antworten auf eigene Mitteilungen – andere Programme senden auf demselben Bus.
                let Some(id) = shown.remove(&args.id) else { continue };
                let accept = match args.action_key.as_str() {
                    "accept" => true,
                    "decline" => false,
                    _ => continue,
                };
                let _ = daemon.request(Request::DropRespond { id, accept, block: false }).await;
            }
        }
    }
}
