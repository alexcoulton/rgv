#![no_main]

use libfuzzer_sys::fuzz_target;
use rgv_hts::{AlignmentOpenOptions, LocalAlignmentSource};

fuzz_target!(|data: &[u8]| {
    if data.len() > 2 * 1024 * 1024 {
        return;
    }
    if let Ok(directory) = tempfile::tempdir() {
        let path = directory.path().join("input.bam");
        if std::fs::write(&path, data).is_ok() {
            let _ = LocalAlignmentSource::open(path, &AlignmentOpenOptions::default());
        }
    }
});
