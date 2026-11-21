//! Pure redaction helpers applied to viewer text before it reaches an MCP client.
//!
//! Viewer diagnostics are passed through so an agent can act on the real failure, but absolute
//! filesystem locations, URL paths/queries/credentials, bearer tokens, and secret-named
//! `key=value` pairs never leave the adapter. Read identities in inlined exports are replaced by
//! export-local pseudonyms unless the caller explicitly opts in.

use std::collections::HashMap;

/// Replacement for the directory part of an absolute filesystem path.
pub(crate) const REDACTED_DIRECTORY: &str = "[path]";
/// Replacement for a credential, token, or URL remainder.
pub(crate) const REDACTED_VALUE: &str = "[redacted]";
/// Upper bound for one diagnostic passed through to the MCP client.
pub(crate) const MAXIMUM_DIAGNOSTIC_BYTES: usize = 4 * 1024;

const TRUNCATION_MARKER: &str = " [truncated]";
const SECRET_KEY_FRAGMENTS: [&str; 10] = [
    "token",
    "secret",
    "password",
    "passwd",
    "signature",
    "credential",
    "apikey",
    "api_key",
    "access_key",
    "authorization",
];

/// Redacts one viewer diagnostic.
///
/// * Absolute paths (`/a/b/c.bam`, `~/c.bam`, `C:\a\c.bam`, `\\host\share\c.bam`) keep only their
///   final component, as `[path]/c.bam`, so "which file" stays diagnosable while the directory
///   layout and home directory do not leak. A path ending in a separator becomes `[path]`.
/// * URLs keep scheme and host only; user information, path, query, and fragment are dropped.
///   `file:` URLs are treated as paths.
/// * `Bearer <token>` and `secret-like-key=<value>` lose their value.
/// * The result is bounded to [`MAXIMUM_DIAGNOSTIC_BYTES`].
pub(crate) fn redact_message(message: &str) -> String {
    let mut output = String::with_capacity(message.len().min(MAXIMUM_DIAGNOSTIC_BYTES));
    let mut previous: Option<char> = None;
    let mut rest = message;
    while let Some(current) = rest.chars().next() {
        if at_boundary(previous)
            && let Some((consumed, replacement)) = sensitive_span(rest, previous)
        {
            output.push_str(&replacement);
            previous = rest[..consumed].chars().next_back();
            rest = &rest[consumed..];
            continue;
        }
        output.push(current);
        previous = Some(current);
        rest = &rest[current.len_utf8()..];
    }
    truncate_diagnostic(output)
}

fn truncate_diagnostic(mut message: String) -> String {
    if message.len() <= MAXIMUM_DIAGNOSTIC_BYTES {
        return message;
    }
    let mut end = MAXIMUM_DIAGNOSTIC_BYTES - TRUNCATION_MARKER.len();
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push_str(TRUNCATION_MARKER);
    message
}

fn at_boundary(previous: Option<char>) -> bool {
    previous.is_none_or(|character| {
        character.is_whitespace()
            || matches!(
                character,
                '"' | '\'' | '`' | '=' | ':' | '(' | '[' | '<' | '{' | ',' | ';' | '&' | '?'
            )
    })
}

/// Returns the byte length of a sensitive span starting at `rest` and its replacement.
fn sensitive_span(rest: &str, previous: Option<char>) -> Option<(usize, String)> {
    if let Some(consumed) = bearer_prefix(rest) {
        let token = unquoted_span(&rest[consumed..]);
        if !token.is_empty() {
            return Some((
                consumed + token.len(),
                format!("{}{REDACTED_VALUE}", &rest[..consumed]),
            ));
        }
    }
    if let Some(key_length) = secret_assignment_prefix(rest) {
        let value = unquoted_span(&rest[key_length..]);
        let value = value.split('&').next().unwrap_or(value);
        if !value.is_empty() {
            return Some((
                key_length + value.len(),
                format!("{}{REDACTED_VALUE}", &rest[..key_length]),
            ));
        }
    }
    let is_url = url_scheme_length(rest).is_some();
    if !is_url && !starts_absolute_path(rest) {
        return None;
    }
    let span = match previous {
        Some(quote @ ('"' | '\'' | '`')) => quoted_span(rest, quote),
        _ => None,
    }
    .unwrap_or_else(|| unquoted_span(rest));
    if span.is_empty() {
        return None;
    }
    let replacement = if is_url {
        redact_url(span)
    } else {
        redact_path(span)
    };
    Some((span.len(), replacement))
}

fn bearer_prefix(rest: &str) -> Option<usize> {
    const PREFIX: &str = "bearer ";
    rest.get(..PREFIX.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
        .map(str::len)
}

/// Matches `identifier=` where the identifier names a credential; returns the prefix length
/// including the `=`.
fn secret_assignment_prefix(rest: &str) -> Option<usize> {
    let key_length = rest
        .find(|character: char| {
            !(character.is_ascii_alphanumeric() || character == '_' || character == '-')
        })
        .filter(|length| *length > 0)?;
    if !rest[key_length..].starts_with('=') {
        return None;
    }
    let key = rest[..key_length].to_ascii_lowercase();
    SECRET_KEY_FRAGMENTS
        .iter()
        .any(|fragment| key.contains(fragment))
        .then_some(key_length + 1)
}

fn url_scheme_length(rest: &str) -> Option<usize> {
    let scheme_length = rest
        .find(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-'))
        })
        .filter(|length| *length > 0)?;
    (rest.as_bytes()[0].is_ascii_alphabetic() && rest[scheme_length..].starts_with("://"))
        .then_some(scheme_length)
}

fn starts_absolute_path(rest: &str) -> bool {
    let bytes = rest.as_bytes();
    let component_start = |byte: Option<&u8>| {
        byte.is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'/' | b'\\'))
    };
    match bytes {
        [b'/', ..] => component_start(bytes.get(1)),
        [b'~', b'/' | b'\\', ..] => true,
        [b'\\', b'\\', ..] => component_start(bytes.get(2)),
        [drive, b':', b'/' | b'\\', ..] => drive.is_ascii_alphabetic(),
        _ => false,
    }
}

/// A quoted path may contain spaces; it ends at the matching quote on the same line.
fn quoted_span(rest: &str, quote: char) -> Option<&str> {
    let end = rest.find(quote)?;
    let span = &rest[..end];
    (!span.contains('\n')).then_some(span)
}

/// An unquoted span ends at whitespace; trailing sentence punctuation is not part of it.
fn unquoted_span(rest: &str) -> &str {
    let end = rest
        .find(|character: char| character.is_whitespace() || matches!(character, '"' | '`'))
        .unwrap_or(rest.len());
    rest[..end].trim_end_matches(['.', ',', ';', ':', ')', ']', '}', '>', '\''])
}

fn redact_path(path: &str) -> String {
    // A trailing separator marks a directory, whose own name (for example a home directory)
    // is not needed to diagnose the failure.
    let name = path.rsplit(['/', '\\']).next().unwrap_or_default();
    if name.is_empty() || name == "~" || name.ends_with(':') {
        REDACTED_DIRECTORY.to_owned()
    } else {
        format!("{REDACTED_DIRECTORY}/{name}")
    }
}

fn redact_url(span: &str) -> String {
    let Ok(parsed) = url::Url::parse(span) else {
        return format!("[url {REDACTED_VALUE}]");
    };
    if parsed.scheme().eq_ignore_ascii_case("file") {
        return redact_path(parsed.path());
    }
    match parsed.host_str() {
        Some(host) => {
            let port = parsed
                .port()
                .map_or_else(String::new, |port| format!(":{port}"));
            format!("{}://{host}{port}/{REDACTED_VALUE}", parsed.scheme())
        }
        None => format!("[url {REDACTED_VALUE}]"),
    }
}

/// How read identities were treated in an inlined export.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadNameHandling {
    /// The export has no read-identity column or field.
    NotPresent,
    /// Read identities were replaced; the on-disk artifact is unchanged.
    Pseudonymized,
    /// The caller explicitly opted in to original read identities.
    Included,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ExportReadNames {
    pub(crate) text: String,
    pub(crate) handling: ReadNameHandling,
    /// Number of distinct read identities replaced (zero unless pseudonymized).
    pub(crate) distinct_read_names: usize,
}

const READ_NAME_COLUMN: &str = "read_name";
const SELECTED_EVIDENCE_HEADER: &str = "section\tkey\tvalue";

fn is_read_identity_key(key: &str) -> bool {
    let key = key.trim().to_ascii_lowercase();
    matches!(
        key.as_str(),
        "read" | "read name" | "read_name" | "template identity" | "qname"
    )
}

/// Replaces read identities in an inlined RGV text export.
///
/// Tabular exports (`# comment` lines, one tab-separated header, then rows) have every value in
/// their `read_name` column replaced by an export-local pseudonym (`read_1`, `read_2`, ... in
/// first-appearance order), so mates and supplementary segments of one template still share an
/// identity. Selected-evidence exports (`section\tkey\tvalue`) have their read-identity rows
/// replaced by `[redacted]`. Everything else, including line structure, is preserved.
pub(crate) fn inline_export_read_names(text: &str, include_read_names: bool) -> ExportReadNames {
    let mut lines = text.split('\n');
    let mut output = Vec::new();
    let mut header = None;
    for line in lines.by_ref() {
        output.push(line.to_owned());
        if !line.starts_with('#') && !line.trim().is_empty() {
            header = Some(line.trim_end_matches('\r').to_owned());
            break;
        }
    }
    let read_name_column = header.as_deref().and_then(|header| {
        header
            .split('\t')
            .position(|column| column.eq_ignore_ascii_case(READ_NAME_COLUMN))
    });
    let selected_evidence = header.as_deref() == Some(SELECTED_EVIDENCE_HEADER);
    let mut pseudonyms = HashMap::<String, usize>::new();
    let mut found = false;
    for line in lines {
        if line.starts_with('#') || line.is_empty() || include_read_names {
            found |= !line.starts_with('#')
                && !line.is_empty()
                && row_has_read_identity(line, read_name_column, selected_evidence);
            output.push(line.to_owned());
            continue;
        }
        let mut fields = line.split('\t').map(str::to_owned).collect::<Vec<_>>();
        if let Some(column) = read_name_column
            && let Some(field) = fields.get_mut(column)
        {
            let next = pseudonyms.len() + 1;
            let identity = *pseudonyms.entry(field.clone()).or_insert(next);
            *field = format!("read_{identity}");
            found = true;
        } else if selected_evidence && fields.len() >= 3 && is_read_identity_key(&fields[1]) {
            pseudonyms.entry(fields[2..].join("\t")).or_insert(0);
            fields.truncate(2);
            fields.push(REDACTED_VALUE.to_owned());
            found = true;
        }
        output.push(fields.join("\t"));
    }
    let handling = match (found, include_read_names) {
        (false, _) => ReadNameHandling::NotPresent,
        (true, true) => ReadNameHandling::Included,
        (true, false) => ReadNameHandling::Pseudonymized,
    };
    ExportReadNames {
        text: output.join("\n"),
        handling,
        distinct_read_names: pseudonyms.len(),
    }
}

fn row_has_read_identity(
    line: &str,
    read_name_column: Option<usize>,
    selected_evidence: bool,
) -> bool {
    let mut fields = line.split('\t');
    match read_name_column {
        Some(column) => fields.nth(column).is_some(),
        None => selected_evidence && fields.nth(1).is_some_and(is_read_identity_key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_and_home_paths_keep_only_the_file_name() {
        assert_eq!(
            redact_message("could not open /Users/alice/patients/NA12878.bam: No such file"),
            "could not open [path]/NA12878.bam: No such file"
        );
        assert_eq!(
            redact_message("index ~/data/sample.bam.bai is stale."),
            "index [path]/sample.bam.bai is stale."
        );
        assert_eq!(redact_message("root is /"), "root is /");
        assert_eq!(
            redact_message("cannot list /home/alice/"),
            "cannot list [path]"
        );
    }

    #[test]
    fn quoted_paths_with_spaces_are_redacted_as_one_span() {
        assert_eq!(
            redact_message(r#"failed to read "/Volumes/My Disk/run 7/reads.cram" (os error 2)"#),
            r#"failed to read "[path]/reads.cram" (os error 2)"#
        );
        assert_eq!(
            redact_message("path='/srv/a b/c.vcf', retry"),
            "path='[path]/c.vcf', retry"
        );
    }

    #[test]
    fn windows_and_unc_paths_are_redacted() {
        assert_eq!(
            redact_message(r"cannot open C:\Users\alice\data\tumour.bam"),
            "cannot open [path]/tumour.bam"
        );
        assert_eq!(
            redact_message(r"cannot open \\fileserver\share\cohort\x.bed, skipping"),
            "cannot open [path]/x.bed, skipping"
        );
    }

    #[test]
    fn urls_keep_host_but_lose_credentials_path_and_query() {
        let redacted = redact_message(
            "GET https://user:hunter2@bucket.example.org:8443/private/s.bam?X-Amz-Signature=abc123 failed",
        );
        assert_eq!(
            redacted,
            "GET https://bucket.example.org:8443/[redacted] failed"
        );
        assert_eq!(
            redact_message("source file:///Users/alice/x.bam missing"),
            "source [path]/x.bam missing"
        );
    }

    #[test]
    fn bearer_tokens_and_secret_assignments_are_removed() {
        let redacted = redact_message(
            "rejected Authorization: Bearer 0123456789abcdef0123456789abcdef; token=zzz&track=4 api_key=k1",
        );
        assert!(!redacted.contains("0123456789abcdef"));
        assert!(!redacted.contains("zzz"));
        assert!(!redacted.contains("k1"));
        assert!(redacted.contains("Bearer [redacted]"));
        assert!(redacted.contains("token=[redacted]&track=4"));
        assert!(redacted.contains("api_key=[redacted]"));
    }

    #[test]
    fn genomic_and_ordinary_diagnostics_pass_through_unchanged() {
        for message in [
            "timeout_millis must be between 100 and 600000",
            "locus chr1:1,000,001-1,100,000 is outside contig chr1 (length 248956422)",
            "track 7 is not an alignment track",
            "expected 1/2 reads and/or mates; ratio 3 / 4",
            "tag grouping requires a two-character SAM tag",
            "exports/reads.tsv is not writable",
        ] {
            assert_eq!(redact_message(message), message);
        }
    }

    #[test]
    fn diagnostics_are_bounded_on_a_character_boundary() {
        let redacted = redact_message(&"é".repeat(MAXIMUM_DIAGNOSTIC_BYTES));
        assert!(redacted.len() <= MAXIMUM_DIAGNOSTIC_BYTES);
        assert!(redacted.ends_with(TRUNCATION_MARKER));
    }

    const ALIGNMENT_EXPORT: &str = "# RGV 0.1.0 alignment (filtered view) export v1\n# genome\thg38\ntrack_id\ttrack_name\tcontig\tstart\tend\tread_name\tflags\tmapq\n1\ttumour\tchr1\t101\t250\tm64011_190830/1/ccs\t0x0063\t60\n1\ttumour\tchr1\t140\t290\tA00123:55:HXXX:1:1101:1000:2000\t0x0093\t60\n1\ttumour\tchr1\t400\t550\tm64011_190830/1/ccs\t0x0800\t60\n# export_truncated\tbyte_limit_reached\n";

    #[test]
    fn tabular_export_read_names_become_stable_pseudonyms() {
        let result = inline_export_read_names(ALIGNMENT_EXPORT, false);
        assert_eq!(result.handling, ReadNameHandling::Pseudonymized);
        assert_eq!(result.distinct_read_names, 2);
        assert!(!result.text.contains("m64011"));
        assert!(!result.text.contains("A00123"));
        let rows = result
            .text
            .lines()
            .filter(|line| line.starts_with("1\t"))
            .map(|line| line.split('\t').collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert_eq!(rows[0][5], "read_1");
        assert_eq!(rows[1][5], "read_2");
        assert_eq!(rows[2][5], "read_1", "segments of one template stay linked");
        assert_eq!(rows[0][6], "0x0063");
        assert_eq!(rows[2][3], "400");
        // Comments, the header, and the line structure are untouched.
        assert_eq!(
            result.text.lines().count(),
            ALIGNMENT_EXPORT.lines().count()
        );
        assert!(result.text.contains("\tread_name\t"));
        assert!(
            result
                .text
                .ends_with("# export_truncated\tbyte_limit_reached\n")
        );
    }

    #[test]
    fn opting_in_returns_the_export_verbatim() {
        let result = inline_export_read_names(ALIGNMENT_EXPORT, true);
        assert_eq!(result.text, ALIGNMENT_EXPORT);
        assert_eq!(result.handling, ReadNameHandling::Included);
        assert_eq!(result.distinct_read_names, 0);
    }

    #[test]
    fn selected_evidence_read_identity_rows_are_redacted() {
        let export = "# RGV 0.1.0 selected evidence export v1\nsection\tkey\tvalue\ndetail\tTrack\ttumour\ndetail\tRead\tm64011_190830/1/ccs\ndetail\tMapping quality\t60\nevidence\tcigar\t150M\n";
        let result = inline_export_read_names(export, false);
        assert_eq!(result.handling, ReadNameHandling::Pseudonymized);
        assert_eq!(result.distinct_read_names, 1);
        assert!(!result.text.contains("m64011"));
        assert!(result.text.contains("detail\tRead\t[redacted]\n"));
        assert!(result.text.contains("detail\tMapping quality\t60\n"));
        assert!(result.text.contains("evidence\tcigar\t150M\n"));
        assert_eq!(inline_export_read_names(export, true).text, export);
    }

    #[test]
    fn exports_without_read_identity_are_unchanged() {
        let pileup =
            "# RGV 0.1.0 pileup export v1\ntrack_id\tcontig\tposition\tdepth\n1\tchr1\t101\t30\n";
        let result = inline_export_read_names(pileup, false);
        assert_eq!(result.text, pileup);
        assert_eq!(result.handling, ReadNameHandling::NotPresent);
        let bed = "chr1\t100\t200\tregion\n";
        assert_eq!(inline_export_read_names(bed, false).text, bed);
        assert_eq!(inline_export_read_names("", false).text, "");
    }
}
