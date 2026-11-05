//! IPC-Frames: Länge + JSON. Darf bei beliebigen Bytes weder abstürzen noch mehr als
//! das Limit an Speicher anfordern.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::ipc::{read_frame, Envelope, Response};

fuzz_target!(|data: &[u8]| {
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(async {
        let mut r = data;
        let _ = read_frame::<_, Envelope>(&mut r).await;
        let mut r = data;
        let _ = read_frame::<_, Response>(&mut r).await;
    });
});
