#![no_main]

use std::io::{Cursor, Write};

use libfuzzer_sys::fuzz_target;
use rgv_hts::{FeatureFormat, TdfSource, TextFeatureSource, TextGwasSource};

fuzz_target!(|data: &[u8]| {
    for format in [
        FeatureFormat::Bed,
        FeatureFormat::Gff3,
        FeatureFormat::Gtf,
        FeatureFormat::GenePred,
        FeatureFormat::RefFlat,
        FeatureFormat::NarrowPeak,
        FeatureFormat::BroadPeak,
        FeatureFormat::Psl,
    ] {
        let _ = TextFeatureSource::from_reader(Cursor::new(data), format);
    }
    let _ = TextGwasSource::from_reader(Cursor::new(data));
    if let Ok(mut file) = tempfile::NamedTempFile::new()
        && file.write_all(data).is_ok()
    {
        let _ = TdfSource::open(file.path());
    }
});
