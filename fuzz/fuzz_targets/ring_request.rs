//! Gerät finden: Nachrichten gekoppelter (evtl. manipulierter) Geräte.
//! Invarianten: nie eine Panik; angenommen wird nur genau `{"v":1,"stop":<bool>}` (keine
//! zusätzlichen Felder wie eine eigene Dauer), und die Prüfung ist stabil.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::ring::decode;

fuzz_target!(|data: &[u8]| {
    if let Some(stop) = decode(data) {
        let v: serde_json::Value = serde_json::from_slice(data).expect("angenommen heißt gültiges JSON");
        let obj = v.as_object().expect("ein Objekt");
        assert!(obj.keys().all(|k| k == "v" || k == "stop"), "keine fremden Felder: {obj:?}");
        assert_eq!(obj.get("v").and_then(|v| v.as_u64()), Some(1));
        let again = serde_json::json!({ "v": 1, "stop": stop }).to_string();
        assert_eq!(decode(again.as_bytes()), Some(stop), "stabil");
    }
});
