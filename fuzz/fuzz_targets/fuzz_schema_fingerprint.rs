#![no_main]
use libfuzzer_sys::fuzz_target;
use conproxy::proxy::types::SchemaFingerprint;

fuzz_target!(|data: &[u8]| {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) {
        let _ = SchemaFingerprint::extract_from_json(&value);
    }
});
