#![no_main]

use libfuzzer_sys::fuzz_target;
use rgv_hts::AuxPredicate;

fuzz_target!(|data: &[u8]| {
    if let Ok(expression) = std::str::from_utf8(data) {
        let _ = expression.parse::<AuxPredicate>();
    }
});
