//! Live-Nachrichten kommen von gekoppelten (ggf. kompromittierten) Geräten: Frames mit
//! beliebigen Bytes dürfen weder abstürzen noch mehr als das Limit an Speicher anfordern.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = space_core::live::decode_message(data);
    // Auch als vollständiger Frame (Länge + Inhalt) über den IPC-Leser mit kleinem Limit
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(async {
        let mut r = data;
        let _ = space_core::ipc::read_frame_max::<_, serde_json::Value>(&mut r, 4096).await;
    });
});
