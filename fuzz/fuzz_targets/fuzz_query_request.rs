#![no_main]
use libfuzzer_sys::fuzz_target;
use conproxy::proxy::types::QueryRequest;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<QueryRequest>(data);
});
