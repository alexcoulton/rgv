//! Remote alignment URLs that do not name their format are identified by the object's magic
//! bytes and read by the pure-Rust range path; `HTSlib` never opens a URL.

use std::{fs, path::Path};

use super::{
    BamFixture,
    differential::{
        CHR1_LENGTH, assert_queries_identical, interval, option_variants, write_bam_fixture,
        write_cram_fixture,
    },
};
use crate::{
    AlignmentError, AlignmentOpenOptions, AlignmentSource, LocalAlignmentSource,
    LocalCramAlignmentSource, ProductionAlignmentSource,
};

/// Serves `objects` by path and returns the fixture with the URL prefix of its server.
fn serve(objects: Vec<(&str, Vec<u8>)>) -> Option<(BamFixture, String)> {
    let fixture = BamFixture::start_with_indexes(
        b"placeholder".to_vec(),
        objects
            .into_iter()
            .map(|(path, bytes)| (path.to_owned(), bytes))
            .collect(),
        "/unused",
    )?;
    let origin = fixture
        .bam_url
        .split("/fixture.bam")
        .next()
        .unwrap()
        .to_owned();
    Some((fixture, origin))
}

fn requested_paths(fixture: &BamFixture) -> Vec<String> {
    fixture
        .requested_urls()
        .into_iter()
        .map(|url| url.split('?').next().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn an_extensionless_bam_url_with_a_query_string_is_sniffed_and_read_without_htslib() {
    let directory = tempfile::tempdir().unwrap();
    let (path, index) = write_bam_fixture(directory.path());
    let Some((fixture, origin)) = serve(vec![
        ("/store/objects/7f3a", fs::read(&path).unwrap()),
        ("/store/objects/7f3a.bai", fs::read(&index).unwrap()),
    ]) else {
        return;
    };
    // The query names another format; only the path and the object's bytes decide.
    let url = format!("{origin}/store/objects/7f3a?X-Signature=abc&filename=reads.cram");
    let mut remote =
        ProductionAlignmentSource::open(Path::new(&url), &AlignmentOpenOptions::default()).unwrap();
    assert!(matches!(remote, ProductionAlignmentSource::RemoteBam(_)));
    assert!(remote.is_remote());
    assert!(remote.remote_io_stats().unwrap().requests > 0);
    // The conventional index was discovered next to the object, keeping the query.
    assert!(
        fixture
            .requested_urls()
            .iter()
            .any(|requested| requested.starts_with("/store/objects/7f3a.bai?X-Signature=abc"))
    );
    let mut local = LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
    for (name, options) in option_variants().into_iter().take(2) {
        let target = interval(0, 0, CHR1_LENGTH);
        let expected = local.query(target, options.clone(), &|| true).unwrap();
        let actual = remote.query(target, options, &|| true).unwrap();
        assert_queries_identical(&expected, &actual, name);
    }
    // Every request the open and the queries made went to the fixture server's log: the
    // object and its index, and nothing else.
    assert!(
        requested_paths(&fixture)
            .iter()
            .all(|path| path.starts_with("/store/objects/7f3a"))
    );
}

#[test]
fn an_extensionless_bam_url_accepts_an_explicit_index_url() {
    let directory = tempfile::tempdir().unwrap();
    let (path, index) = write_bam_fixture(directory.path());
    let Some((fixture, origin)) = serve(vec![
        ("/download", fs::read(&path).unwrap()),
        ("/indexes/42", fs::read(&index).unwrap()),
    ]) else {
        return;
    };
    let options = AlignmentOpenOptions {
        remote_index_url: Some(format!("{origin}/indexes/42?token=index")),
        ..AlignmentOpenOptions::default()
    };
    let url = format!("{origin}/download?id=42&token=source");
    let mut remote = ProductionAlignmentSource::open(Path::new(&url), &options).unwrap();
    assert!(matches!(remote, ProductionAlignmentSource::RemoteBam(_)));
    let query = remote
        .query(
            interval(0, 4_990, 5_300),
            option_variants()[1].1.clone(),
            &|| true,
        )
        .unwrap();
    // Both overlapping mates, both same-start mates and the long-CIGAR read spanning them.
    assert_eq!(query.stats.records_eligible, 5);
    assert_eq!(
        requested_paths(&fixture)
            .into_iter()
            .filter(|path| path != "/download" && path != "/indexes/42")
            .collect::<Vec<_>>(),
        Vec::<String>::new()
    );
}

#[test]
fn an_extensionless_cram_url_is_sniffed_and_matches_the_local_cram_decoder() {
    let directory = tempfile::tempdir().unwrap();
    let (path, reference) = write_cram_fixture(directory.path());
    let Some((fixture, origin)) = serve(vec![
        ("/bucket/sample-1", fs::read(&path).unwrap()),
        (
            "/bucket/sample-1.crai",
            fs::read(format!("{}.crai", path.display())).unwrap(),
        ),
    ]) else {
        return;
    };
    let options = AlignmentOpenOptions {
        reference_path: Some(reference),
        ..AlignmentOpenOptions::default()
    };
    let url = format!("{origin}/bucket/sample-1?versionId=3&format=bam");
    let mut remote = ProductionAlignmentSource::open(Path::new(&url), &options).unwrap();
    assert!(matches!(remote, ProductionAlignmentSource::RemoteCram(_)));
    assert!(remote.is_cram());
    let mut local = LocalCramAlignmentSource::open(&path, &options).unwrap();
    for (name, query_options) in option_variants().into_iter().take(2) {
        let target = interval(0, 0, CHR1_LENGTH);
        let expected = local
            .query(target, query_options.clone(), &|| true)
            .unwrap();
        let actual = remote.query(target, query_options, &|| true).unwrap();
        assert_queries_identical(&expected, &actual, name);
    }
    assert!(
        requested_paths(&fixture)
            .iter()
            .all(|path| path.starts_with("/bucket/sample-1"))
    );
}

#[test]
fn an_unrecognised_remote_object_fails_explicitly_and_htslib_refuses_urls() {
    let Some((fixture, origin)) = serve(vec![
        ("/notes", b"@HD\tVN:1.6\n".to_vec()),
        ("/gzip", {
            use std::io::Write as _;
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(b"not an alignment").unwrap();
            encoder.finish().unwrap()
        }),
        ("/empty", Vec::new()),
    ]) else {
        return;
    };
    for object in ["/notes", "/gzip"] {
        let url = format!("{origin}{object}?token=1");
        let error =
            ProductionAlignmentSource::open(Path::new(&url), &AlignmentOpenOptions::default())
                .err()
                .unwrap_or_else(|| panic!("{object} opened as an alignment"));
        assert!(
            matches!(&error, AlignmentError::Io(error) if error.to_string().contains("neither BAM nor CRAM")),
            "{object}: {error}"
        );
    }
    assert!(
        ProductionAlignmentSource::open(
            Path::new(&format!("{origin}/empty")),
            &AlignmentOpenOptions::default()
        )
        .is_err()
    );
    // No index or companion object was requested for something that is not an alignment.
    assert!(
        requested_paths(&fixture)
            .iter()
            .all(|path| ["/notes", "/gzip", "/empty"].contains(&path.as_str()))
    );
    // The HTSlib source itself refuses a URL instead of handing it to libcurl.
    let before = fixture.requested_urls().len();
    let error = LocalAlignmentSource::open(
        format!("{origin}/notes.bam"),
        &AlignmentOpenOptions::default(),
    )
    .err()
    .expect("HTSlib opened a URL");
    assert!(matches!(error, AlignmentError::Io(_)), "{error}");
    assert_eq!(fixture.requested_urls().len(), before);
    for url in [
        "ftp://example.invalid/reads.bam",
        "s3://bucket/reads.bam",
        "gs://bucket/reads.cram",
    ] {
        let error = LocalAlignmentSource::open(url, &AlignmentOpenOptions::default())
            .err()
            .expect("HTSlib opened a URL");
        assert!(
            matches!(&error, AlignmentError::Io(error) if error.kind() == std::io::ErrorKind::InvalidInput),
            "{url}: {error}"
        );
    }
}
