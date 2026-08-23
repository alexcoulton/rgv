use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAXIMUM_SAMPLE_METADATA_BYTES: usize = 64 * 1024 * 1024;
pub const MAXIMUM_SAMPLE_METADATA_ROWS: usize = 100_000;
pub const MAXIMUM_SAMPLE_METADATA_COLUMNS: usize = 256;
pub const MAXIMUM_SAMPLE_METADATA_CELLS: usize = 1_000_000;
pub const MAXIMUM_SAMPLE_METADATA_FIELD_BYTES: usize = 16_384;
pub const MAXIMUM_SAMPLE_KEY_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SampleMetadataRow {
    pub sample_key: String,
    pub values: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SampleMetadataTable {
    pub key_column: String,
    pub columns: Vec<String>,
    pub rows: Vec<SampleMetadataRow>,
}

impl SampleMetadataTable {
    pub fn from_tsv(text: &str) -> Result<Self, SampleMetadataError> {
        if text.len() > MAXIMUM_SAMPLE_METADATA_BYTES {
            return Err(SampleMetadataError::TooManyBytes);
        }
        let mut lines = text.lines();
        let header = lines.next().ok_or(SampleMetadataError::MissingHeader)?;
        let header = header.trim_end_matches('\r');
        let fields = header.split('\t').collect::<Vec<_>>();
        if fields.is_empty() || fields.len() > MAXIMUM_SAMPLE_METADATA_COLUMNS + 1 {
            return Err(SampleMetadataError::InvalidColumns);
        }
        let key_column = validate_header(fields[0])?;
        let mut seen_columns = HashSet::new();
        seen_columns.insert(key_column.to_ascii_lowercase());
        let mut columns = Vec::with_capacity(fields.len().saturating_sub(1));
        for field in fields.iter().skip(1) {
            let column = validate_header(field)?;
            if !seen_columns.insert(column.to_ascii_lowercase()) {
                return Err(SampleMetadataError::DuplicateColumn(column));
            }
            columns.push(column);
        }

        let mut rows = Vec::new();
        let mut seen_keys = HashSet::new();
        for (offset, line) in lines.enumerate() {
            let line_number = offset + 2;
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if rows.len() >= MAXIMUM_SAMPLE_METADATA_ROWS {
                return Err(SampleMetadataError::TooManyRows);
            }
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() != columns.len() + 1 {
                return Err(SampleMetadataError::WrongFieldCount { line: line_number });
            }
            let sample_key = fields[0].trim();
            if sample_key.is_empty() || sample_key.len() > MAXIMUM_SAMPLE_KEY_BYTES {
                return Err(SampleMetadataError::InvalidSampleKey { line: line_number });
            }
            if !seen_keys.insert(sample_key.to_owned()) {
                return Err(SampleMetadataError::DuplicateSampleKey(
                    sample_key.to_owned(),
                ));
            }
            let values = fields
                .iter()
                .skip(1)
                .map(|value| {
                    if value.len() > MAXIMUM_SAMPLE_METADATA_FIELD_BYTES {
                        Err(SampleMetadataError::FieldTooLong { line: line_number })
                    } else {
                        Ok((*value).to_owned())
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            rows.push(SampleMetadataRow {
                sample_key: sample_key.to_owned(),
                values,
            });
            if rows.len().saturating_mul(columns.len()) > MAXIMUM_SAMPLE_METADATA_CELLS {
                return Err(SampleMetadataError::TooManyCells);
            }
        }
        let table = Self {
            key_column,
            columns,
            rows,
        };
        table.validate()?;
        Ok(table)
    }

    pub fn validate(&self) -> Result<(), SampleMetadataError> {
        if self.rows.len() > MAXIMUM_SAMPLE_METADATA_ROWS
            || self.columns.len() > MAXIMUM_SAMPLE_METADATA_COLUMNS
            || self.rows.len().saturating_mul(self.columns.len()) > MAXIMUM_SAMPLE_METADATA_CELLS
        {
            return Err(SampleMetadataError::InvalidBounds);
        }
        validate_header(&self.key_column)?;
        let mut columns = HashSet::new();
        columns.insert(self.key_column.to_ascii_lowercase());
        for column in &self.columns {
            validate_header(column)?;
            if !columns.insert(column.to_ascii_lowercase()) {
                return Err(SampleMetadataError::DuplicateColumn(column.clone()));
            }
        }
        let mut keys = HashSet::new();
        for row in &self.rows {
            if row.sample_key.trim().is_empty()
                || row.sample_key.len() > MAXIMUM_SAMPLE_KEY_BYTES
                || row.sample_key.contains(['\t', '\r', '\n'])
                || !keys.insert(row.sample_key.clone())
                || row.values.len() != self.columns.len()
                || row
                    .values
                    .iter()
                    .any(|value| value.len() > MAXIMUM_SAMPLE_METADATA_FIELD_BYTES)
            {
                return Err(SampleMetadataError::InvalidRows);
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn row(&self, sample_key: &str) -> Option<&SampleMetadataRow> {
        self.rows.iter().find(|row| row.sample_key == sample_key)
    }

    #[must_use]
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|column| column.eq_ignore_ascii_case(name))
    }

    #[must_use]
    pub fn distinct_values(&self, column_index: usize) -> Vec<&str> {
        self.rows
            .iter()
            .filter_map(|row| row.values.get(column_index))
            .map(String::as_str)
            .filter(|value| !value.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[must_use]
    pub fn sample_keys_matching(&self, column_index: usize, value: &str) -> BTreeSet<&str> {
        self.rows
            .iter()
            .filter(|row| {
                row.values
                    .get(column_index)
                    .is_some_and(|candidate| candidate == value)
            })
            .map(|row| row.sample_key.as_str())
            .collect()
    }

    /// Orders header sample indices by family, then child/proband, father,
    /// mother, and other family members. Returns `None` when no recognizable
    /// parent columns are present, leaving callers' source order untouched.
    #[must_use]
    pub fn pedigree_sample_order(&self, samples: &[String]) -> Option<Vec<usize>> {
        let father_column =
            self.column_index_from_aliases(&["father", "father_id", "paternal_id", "paternal"]);
        let mother_column =
            self.column_index_from_aliases(&["mother", "mother_id", "maternal_id", "maternal"]);
        if father_column.is_none() && mother_column.is_none() {
            return None;
        }
        let family_column = self.column_index_from_aliases(&["family_id", "family", "pedigree"]);
        let rows = self
            .rows
            .iter()
            .map(|row| (row.sample_key.as_str(), row))
            .collect::<BTreeMap<_, _>>();
        let mut parent_roles = BTreeMap::<&str, u8>::new();
        for row in &self.rows {
            if let Some(father) = father_column
                .and_then(|index| row.values.get(index))
                .map(String::as_str)
                .filter(|value| !is_missing_parent(value))
            {
                parent_roles
                    .entry(father)
                    .and_modify(|rank| *rank = (*rank).min(1))
                    .or_insert(1);
            }
            if let Some(mother) = mother_column
                .and_then(|index| row.values.get(index))
                .map(String::as_str)
                .filter(|value| !is_missing_parent(value))
            {
                parent_roles.entry(mother).or_insert(2);
            }
        }
        let mut order = (0..samples.len()).collect::<Vec<_>>();
        order.sort_by(|left, right| {
            let key = |index: usize| {
                let sample = samples[index].as_str();
                let row = rows.get(sample).copied();
                let family = row
                    .and_then(|row| family_column.and_then(|column| row.values.get(column)))
                    .filter(|value| !value.is_empty())
                    .map_or(sample, String::as_str);
                let has_parents = row.is_some_and(|row| {
                    [father_column, mother_column]
                        .into_iter()
                        .flatten()
                        .filter_map(|column| row.values.get(column))
                        .any(|value| !is_missing_parent(value))
                });
                let role = if has_parents {
                    0
                } else {
                    parent_roles.get(sample).copied().unwrap_or(3)
                };
                (family, role, index)
            };
            key(*left).cmp(&key(*right))
        });
        Some(order)
    }

    fn column_index_from_aliases(&self, aliases: &[&str]) -> Option<usize> {
        aliases.iter().find_map(|alias| self.column_index(alias))
    }
}

fn validate_header(value: &str) -> Result<String, SampleMetadataError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAXIMUM_SAMPLE_KEY_BYTES
        || value.contains(['\t', '\r', '\n'])
    {
        return Err(SampleMetadataError::InvalidColumns);
    }
    Ok(value.to_owned())
}

fn is_missing_parent(value: &str) -> bool {
    value.is_empty() || matches!(value, "0" | "." | "NA" | "N/A")
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SampleMetadataError {
    #[error("sample metadata exceeds the 64 MiB input limit")]
    TooManyBytes,
    #[error("sample metadata requires a tab-separated header")]
    MissingHeader,
    #[error("sample metadata has invalid, duplicate, or excessive columns")]
    InvalidColumns,
    #[error("sample metadata column {0:?} is duplicated")]
    DuplicateColumn(String),
    #[error("sample metadata exceeds {MAXIMUM_SAMPLE_METADATA_ROWS} rows")]
    TooManyRows,
    #[error("sample metadata exceeds {MAXIMUM_SAMPLE_METADATA_CELLS} attribute cells")]
    TooManyCells,
    #[error("sample metadata line {line} has the wrong field count")]
    WrongFieldCount { line: usize },
    #[error("sample metadata line {line} has an invalid sample key")]
    InvalidSampleKey { line: usize },
    #[error("sample metadata sample key {0:?} is duplicated")]
    DuplicateSampleKey(String),
    #[error("sample metadata line {line} contains a value longer than the field limit")]
    FieldTooLong { line: usize },
    #[error("sample metadata exceeds its serialized bounds")]
    InvalidBounds,
    #[error("sample metadata contains invalid serialized rows")]
    InvalidRows,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsv_preserves_stable_keys_and_generic_attributes() {
        let table = SampleMetadataTable::from_tsv(concat!(
            "sample\tcondition\tbatch\n",
            "normal\tcontrol\tA\n",
            "tumour\tcase\tB\n",
            "relapse\tcase\tB\n",
        ))
        .unwrap();
        assert_eq!(table.key_column, "sample");
        assert_eq!(table.columns, ["condition", "batch"]);
        assert_eq!(table.row("tumour").unwrap().values, ["case", "B"]);
        assert_eq!(table.distinct_values(0), ["case", "control"]);
        assert_eq!(
            table.sample_keys_matching(1, "B"),
            BTreeSet::from(["relapse", "tumour"])
        );
        table.validate().unwrap();
    }

    #[test]
    fn tsv_rejects_duplicate_keys_columns_and_ragged_rows() {
        assert!(matches!(
            SampleMetadataTable::from_tsv("sample\tgroup\tGROUP\na\tx\ty\n"),
            Err(SampleMetadataError::DuplicateColumn(_))
        ));
        assert!(matches!(
            SampleMetadataTable::from_tsv("sample\tgroup\na\tx\na\ty\n"),
            Err(SampleMetadataError::DuplicateSampleKey(_))
        ));
        assert!(matches!(
            SampleMetadataTable::from_tsv("sample\tgroup\na\n"),
            Err(SampleMetadataError::WrongFieldCount { .. })
        ));
    }

    #[test]
    fn pedigree_order_places_child_father_and_mother_within_family() {
        let table = SampleMetadataTable::from_tsv(concat!(
            "sample\tfamily_id\tfather_id\tmother_id\n",
            "mother\tfam1\t0\t0\n",
            "child\tfam1\tfather\tmother\n",
            "father\tfam1\t0\t0\n",
            "unrelated\tfam2\t0\t0\n",
        ))
        .unwrap();
        let samples = vec![
            "father".to_owned(),
            "unrelated".to_owned(),
            "mother".to_owned(),
            "child".to_owned(),
        ];
        assert_eq!(
            table.pedigree_sample_order(&samples),
            Some(vec![3, 0, 2, 1])
        );
    }

    #[test]
    fn pedigree_order_is_absent_without_parent_columns() {
        let table = SampleMetadataTable::from_tsv("sample\tgroup\na\tcase\n").unwrap();
        assert_eq!(table.pedigree_sample_order(&["a".to_owned()]), None);
    }
}
