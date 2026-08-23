#![no_main]

use libfuzzer_sys::fuzz_target;
use rgv_core::SessionDocument;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = SessionDocument::from_json(text);
        let _ = SessionDocument::import_igv(text);
    }
});
