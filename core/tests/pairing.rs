//! Kopplung mit Bestätigung (Prüfcode): Ein abgefangener oder weitergegebener Einladungscode
//! reicht allein nicht – das einladende Gerät koppelt erst nach Bestätigung durch den Nutzer.

use space_core::events::Event;
use space_core::ipc::{Request, Response};
use space_core::{Config, Daemon};
use std::path::Path;
use std::time::Duration;

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn start(root: &Path, name: &str) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = Some(free_port());
    c.connect_timeout_secs = 2;
    c.device_name = name.into();
    c.live_connections = false;
    Daemon::start_loopback(c).await.unwrap()
}

async fn invite(d: &Daemon) -> String {
    match d.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    }
}

async fn wait_for<T>(sub: &mut space_core::events::Subscription, f: impl Fn(&Event) -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(e) = sub.next().await {
                if let Some(v) = f(&e) {
                    return v;
                }
            }
        }
    })
    .await
    .expect("Ereignis kam nicht")
}

async fn devices(d: &Daemon) -> usize {
    match d.request(Request::PairList).await {
        Response::Devices { devices } => devices.len(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gleicher_code_und_erst_nach_bestaetigung() {
    let tmp = tempfile::tempdir().unwrap();
    let host = start(&tmp.path().join("host"), "Laptop").await;
    let guest = start(&tmp.path().join("guest"), "Handy").await;
    let mut hs = host.subscribe();
    let mut gs = guest.subscribe();
    let code = invite(&host).await;
    let join = {
        let g = guest.clone();
        tokio::spawn(async move { g.request(Request::PairJoin { code }).await })
    };
    let (host_code, name) = wait_for(&mut hs, |e| {
        if let Event::PairConfirm { code, name } = e {
            Some((code.clone(), name.clone()))
        } else {
            None
        }
    })
    .await;
    let guest_code =
        wait_for(&mut gs, |e| if let Event::PairCode { code } = e { Some(code.clone()) } else { None }).await;
    assert_eq!(host_code, guest_code, "beide Geräte zeigen denselben Prüfcode");
    assert_eq!(name, "Handy");
    // Vor der Bestätigung ist niemand gekoppelt
    assert_eq!((devices(&host).await, devices(&guest).await), (0, 0));
    assert!(matches!(host.request(Request::PairConfirm { accept: true }).await, Response::Success { .. }));
    assert!(matches!(join.await.unwrap(), Response::Paired { .. }));
    assert_eq!((devices(&host).await, devices(&guest).await), (1, 1));
    // Ohne wartende Anfrage gibt es nichts zu bestätigen
    assert!(matches!(host.request(Request::PairConfirm { accept: true }).await, Response::Error { .. }));
    host.shutdown().await;
    guest.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn abgefangener_code_nutzt_ohne_bestaetigung_nichts() {
    let tmp = tempfile::tempdir().unwrap();
    let host = start(&tmp.path().join("host"), "Laptop").await;
    let mallory = start(&tmp.path().join("mallory"), "Handy von Yanic").await;
    let mut hs = host.subscribe();
    let code = invite(&host).await;
    // Ein Fremder hat den Code abgefangen und kommt dem echten Gerät zuvor
    let join = {
        let m = mallory.clone();
        let c = code.clone();
        tokio::spawn(async move { m.request(Request::PairJoin { code: c }).await })
    };
    wait_for(&mut hs, |e| matches!(e, Event::PairConfirm { .. }).then_some(())).await;
    // Der Nutzer sieht einen Code, der nicht zu seinem Gerät passt, und lehnt ab
    assert!(matches!(host.request(Request::PairConfirm { accept: false }).await, Response::Success { .. }));
    let r = join.await.unwrap();
    assert!(matches!(&r, Response::Error { message, .. } if message.contains("nicht bestätigt")), "{r:?}");
    assert_eq!((devices(&host).await, devices(&mallory).await), (0, 0), "niemand gekoppelt");
    // Der Code ist verbraucht – auch ein zweiter Versuch hilft nicht
    let r = mallory.request(Request::PairJoin { code }).await;
    assert!(matches!(r, Response::Error { .. }), "{r:?}");
    assert_eq!(devices(&host).await, 0);
    host.shutdown().await;
    mallory.shutdown().await;
}
