//! Source predicates are independent of the bounded, retained display payload.
//! `HTSlib` owns each decoded record; evaluation copies at most one sample/field.
use super::*;

fn unavailable(detail: &str) -> TrackError {
    TrackError::ResourceLimit(format!(
        "exact variant filter evaluation unavailable: {detail}"
    ))
}

pub(super) fn validate_filter_authority(
    raw: &bcf::header::HeaderView,
    header: &VariantHeader,
    omitted_samples: usize,
    settings: &VariantTrackSettings,
) -> Result<(), TrackError> {
    let cohort_filter = settings.genotype != VariantGenotypeFilter::Any
        || settings
            .field_predicates
            .iter()
            .any(|p| p.scope == VariantFieldScope::Format);
    if cohort_filter && omitted_samples > 0 {
        return Err(unavailable("sample names exceed the header limit"));
    }
    for predicate in &settings.field_predicates {
        let (schemas, exists) = match predicate.scope {
            VariantFieldScope::Info => (
                &header.info,
                raw.info_type(predicate.key.as_bytes()).is_ok(),
            ),
            VariantFieldScope::Format => (
                &header.formats,
                raw.format_type(predicate.key.as_bytes()).is_ok(),
            ),
        };
        if exists && !schemas.iter().any(|schema| schema.id == predicate.key) {
            return Err(unavailable(
                "required field schema exceeds the header limit",
            ));
        }
    }
    Ok(())
}

pub(super) fn source_record_matches(
    raw: &bcf::Record,
    record: &VariantRecord,
    header: &VariantHeader,
    settings: &VariantTrackSettings,
    samples: &[usize],
) -> Result<bool, TrackError> {
    for field in &record.info {
        let required = (field.key == "AF"
            && (settings.minimum_allele_frequency.is_some()
                || settings.maximum_allele_frequency.is_some()))
            || settings
                .field_predicates
                .iter()
                .any(|p| p.scope == VariantFieldScope::Info && p.key == field.key);
        if required && field.truncated {
            return Err(unavailable("required INFO value exceeds the field limit"));
        }
    }
    let mut record_settings = settings.clone();
    record_settings.genotype = VariantGenotypeFilter::Any;
    record_settings
        .field_predicates
        .retain(|p| p.scope == VariantFieldScope::Info);
    if !variant_record_matches(record, header, &record_settings) {
        return Ok(false);
    }
    if !source_genotypes_match(raw, settings, samples)? {
        return Ok(false);
    }
    source_format_predicates_match(raw, header, settings, samples)
}

fn source_genotypes_match(
    raw: &bcf::Record,
    settings: &VariantTrackSettings,
    samples: &[usize],
) -> Result<bool, TrackError> {
    let needs_gt = settings.genotype != VariantGenotypeFilter::Any
        || settings
            .field_predicates
            .iter()
            .any(|p| p.scope == VariantFieldScope::Format && p.key == "GT");
    if needs_gt {
        let values = optional_source_field(raw.genotypes())?;
        let genotype = |index| -> Result<VariantGenotype, TrackError> {
            let alleles = if let Some(values) = &values {
                let value = values.get(index);
                if value.len() > MAX_VARIANT_VALUES_PER_FIELD {
                    return Err(unavailable("genotype ploidy exceeds the evaluation limit"));
                }
                value
                    .iter()
                    .enumerate()
                    .map(|(position, allele)| VariantGenotypeAllele {
                        index: allele.index(),
                        phased: position > 0
                            && matches!(
                                allele,
                                GenotypeAllele::Phased(_) | GenotypeAllele::PhasedMissing
                            ),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            Ok(VariantGenotype {
                sample_index: index,
                alleles,
                fields: Vec::new(),
            })
        };
        if settings.genotype != VariantGenotypeFilter::Any {
            let mut matched = false;
            for &index in samples {
                if genotype_matches(&genotype(index)?, settings.genotype) {
                    matched = true;
                    break;
                }
            }
            if !matched {
                return Ok(false);
            }
        }
        for predicate in settings
            .field_predicates
            .iter()
            .filter(|p| p.scope == VariantFieldScope::Format && p.key == "GT")
        {
            // GT historically compares a single multi-valued cohort field:
            // NotEquals is ALL called values, all other comparisons use ANY.
            let all = matches!(predicate.comparison, VariantFieldComparison::NotEquals(_));
            let mut present = false;
            let mut matched = all;
            for &index in samples {
                if let Some(value) = genotype_filter_value(&genotype(index)?) {
                    present = true;
                    let result = variant_value_matches(
                        &VariantValue::Strings(vec![value]),
                        &predicate.comparison,
                    );
                    if all {
                        matched &= result;
                    } else {
                        matched |= result;
                    }
                }
            }
            if !(if present {
                matched
            } else {
                predicate.missing == VariantMissingValueRule::Include
            }) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn source_format_predicates_match(
    raw: &bcf::Record,
    header: &VariantHeader,
    settings: &VariantTrackSettings,
    samples: &[usize],
) -> Result<bool, TrackError> {
    for predicate in settings
        .field_predicates
        .iter()
        .filter(|p| p.scope == VariantFieldScope::Format && p.key != "GT")
    {
        let Some(schema) = header.formats.iter().find(|s| s.id == predicate.key) else {
            if predicate.missing == VariantMissingValueRule::Exclude {
                return Ok(false);
            }
            continue;
        };
        let matched = match schema.value_type {
            VariantValueType::Integer => {
                let values = optional_source_field(raw.format(schema.id.as_bytes()).integer())?;
                evaluate_samples(samples, predicate, |index| {
                    let Some(value) = values.as_ref().and_then(|v| v.get(index)) else {
                        return Ok(None);
                    };
                    check_value_count(value.len())?;
                    Ok(Some(VariantValue::Integers(
                        value
                            .iter()
                            .map(|v| (!v.is_missing()).then_some(*v))
                            .collect(),
                    )))
                })?
            }
            VariantValueType::Float => {
                let values = optional_source_field(raw.format(schema.id.as_bytes()).float())?;
                evaluate_samples(samples, predicate, |index| {
                    let Some(value) = values.as_ref().and_then(|v| v.get(index)) else {
                        return Ok(None);
                    };
                    check_value_count(value.len())?;
                    Ok(Some(VariantValue::Floats(
                        value
                            .iter()
                            .map(|v| (!v.is_missing()).then_some(*v))
                            .collect(),
                    )))
                })?
            }
            VariantValueType::String => {
                let values = optional_source_field(raw.format(schema.id.as_bytes()).string())?;
                evaluate_samples(samples, predicate, |index| {
                    let Some(value) = values.as_ref().and_then(|v| v.get(index)) else {
                        return Ok(None);
                    };
                    if value.len() > MAX_VARIANT_STRING_BYTES {
                        return Err(unavailable(
                            "required FORMAT string exceeds the field limit",
                        ));
                    }
                    if *value == b"." {
                        return Ok(None);
                    }
                    Ok(Some(VariantValue::Strings(vec![
                        String::from_utf8_lossy(value).into_owned(),
                    ])))
                })?
            }
            VariantValueType::Flag => {
                return Err(unavailable("FORMAT Flag predicates are unsupported"));
            }
        };
        if !matched {
            return Ok(false);
        }
    }
    Ok(true)
}

fn check_value_count(count: usize) -> Result<(), TrackError> {
    if count > MAX_VARIANT_VALUES_PER_FIELD {
        Err(unavailable("required FORMAT values exceed the field limit"))
    } else {
        Ok(())
    }
}

fn optional_source_field<T>(
    result: Result<T, rust_htslib::errors::Error>,
) -> Result<Option<T>, TrackError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(
            rust_htslib::errors::Error::BcfMissingTag { .. }
            | rust_htslib::errors::Error::BcfUndefinedTag { .. },
        ) => Ok(None),
        Err(error) => Err(TrackError::Backend(format!(
            "variant filter field: {error}"
        ))),
    }
}

fn evaluate_samples(
    samples: &[usize],
    predicate: &VariantFieldPredicate,
    mut value: impl FnMut(usize) -> Result<Option<VariantValue>, TrackError>,
) -> Result<bool, TrackError> {
    let mut present = false;
    for &index in samples {
        if let Some(value) = value(index)? {
            let field = VariantField {
                key: String::new(),
                value,
                truncated: false,
            };
            present |= field_has_value(&field);
            if field_has_value(&field) && variant_value_matches(&field.value, &predicate.comparison)
            {
                return Ok(true);
            }
        }
    }
    Ok(!present && predicate.missing == VariantMissingValueRule::Include)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_htslib::bcf::{Format, Header, Writer};

    #[test]
    fn required_field_truncation_is_an_error_not_a_missing_or_nonmatching_value() {
        let directory = tempfile::tempdir().unwrap();
        let mut header = Header::new();
        header.push_record(br"##contig=<ID=chr1,length=100>");
        header.push_record(br#"##FORMAT=<ID=DP,Number=.,Type=Integer,Description="Depth">"#);
        header.push_sample(b"sample");
        let writer = Writer::from_path(
            directory.path().join("fixture.bcf"),
            &header,
            false,
            Format::Bcf,
        )
        .unwrap();
        let mut raw = writer.empty_record();
        raw.set_rid(Some(0));
        raw.set_pos(0);
        raw.set_alleles(&[b"A", b"C"]).unwrap();
        raw.push_format_integer(b"DP", &vec![42; MAX_VARIANT_VALUES_PER_FIELD + 1])
            .unwrap();
        let (decoded_header, _, _, _) = decode_variant_header(raw.header());
        let (record, _, _) = decode_variant_record(&raw, &decoded_header, "chr1", 0, &[0]).unwrap();
        let settings = VariantTrackSettings {
            field_predicates: vec![VariantFieldPredicate {
                scope: VariantFieldScope::Format,
                key: "DP".into(),
                comparison: VariantFieldComparison::GreaterThan(40.0),
                missing: VariantMissingValueRule::Include,
            }],
            ..VariantTrackSettings::default()
        };
        assert!(matches!(
            source_record_matches(&raw, &record, &decoded_header, &settings, &[0]),
            Err(TrackError::ResourceLimit(_))
        ));
        let mut omitted_header = decoded_header.clone();
        omitted_header.formats.clear();
        assert!(validate_filter_authority(raw.header(), &omitted_header, 0, &settings).is_err());
        assert!(validate_filter_authority(raw.header(), &decoded_header, 1, &settings).is_err());
        raw.push_format_integer(b"DP", &[i32::missing()]).unwrap();
        assert!(source_record_matches(&raw, &record, &decoded_header, &settings, &[0]).unwrap());
        let mut exclude = settings;
        exclude.field_predicates[0].missing = VariantMissingValueRule::Exclude;
        assert!(!source_record_matches(&raw, &record, &decoded_header, &exclude, &[0]).unwrap());
    }
}
