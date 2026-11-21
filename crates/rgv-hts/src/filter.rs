use std::str::FromStr;

use thiserror::Error;

use crate::AuxValue;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlignmentFilter {
    pub minimum_mapping_quality: Option<u8>,
    pub required_flags: u16,
    pub excluded_flags: u16,
    pub auxiliary: Vec<AuxPredicate>,
}

impl AlignmentFilter {
    pub fn fingerprint(&self) -> u64 {
        let mut hash = Fnv64::default();
        hash.write(&[self.minimum_mapping_quality.unwrap_or(u8::MAX)]);
        hash.write(&self.required_flags.to_le_bytes());
        hash.write(&self.excluded_flags.to_le_bytes());
        for predicate in &self.auxiliary {
            hash.write(&predicate.tag);
            hash.write(&[predicate.comparison as u8]);
            match &predicate.value {
                None => hash.write(&[0]),
                Some(FilterValue::Integer(value)) => {
                    hash.write(&[1]);
                    hash.write(&value.to_le_bytes());
                }
                Some(FilterValue::Float(value)) => {
                    hash.write(&[2]);
                    hash.write(&value.to_bits().to_le_bytes());
                }
                Some(FilterValue::String(value)) => {
                    hash.write(&[3]);
                    hash.write(value.as_bytes());
                }
            }
        }
        hash.finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum AuxComparison {
    Exists,
    Equal,
    NotEqual,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FilterValue {
    Integer(i64),
    Float(f64),
    String(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AuxPredicate {
    pub tag: [u8; 2],
    pub comparison: AuxComparison,
    pub value: Option<FilterValue>,
}

impl AuxPredicate {
    pub(crate) fn matches_value(&self, value: &AuxValue) -> bool {
        let Some(expected) = &self.value else {
            return false;
        };
        let Some(actual) = FilterScalar::from_value(value) else {
            return false;
        };
        self.matches_scalar(&actual, expected)
    }

    fn matches_scalar(&self, actual: &FilterScalar<'_>, expected: &FilterValue) -> bool {
        match self.comparison {
            AuxComparison::Exists => true,
            AuxComparison::Equal => actual.equal(expected),
            AuxComparison::NotEqual => !actual.equal(expected),
            AuxComparison::Greater => actual
                .order(expected)
                .is_some_and(std::cmp::Ordering::is_gt),
            AuxComparison::GreaterOrEqual => actual
                .order(expected)
                .is_some_and(std::cmp::Ordering::is_ge),
            AuxComparison::Less => actual
                .order(expected)
                .is_some_and(std::cmp::Ordering::is_lt),
            AuxComparison::LessOrEqual => actual
                .order(expected)
                .is_some_and(std::cmp::Ordering::is_le),
        }
    }
}

impl FromStr for AuxPredicate {
    type Err = FilterParseError;

    fn from_str(expression: &str) -> Result<Self, Self::Err> {
        let expression = expression.trim();
        if expression.len() == 2 {
            return Ok(Self {
                tag: expression
                    .as_bytes()
                    .try_into()
                    .map_err(|_| FilterParseError::InvalidTag)?,
                comparison: AuxComparison::Exists,
                value: None,
            });
        }
        let operators = [
            (">=", AuxComparison::GreaterOrEqual),
            ("<=", AuxComparison::LessOrEqual),
            ("!=", AuxComparison::NotEqual),
            ("=", AuxComparison::Equal),
            (">", AuxComparison::Greater),
            ("<", AuxComparison::Less),
        ];
        let (operator_index, operator, comparison) = operators
            .iter()
            .filter_map(|(operator, comparison)| {
                expression
                    .find(operator)
                    .map(|index| (index, *operator, *comparison))
            })
            .min_by_key(|(index, _, _)| *index)
            .ok_or(FilterParseError::MissingOperator)?;
        let tag = expression[..operator_index].trim();
        let tag: [u8; 2] = tag
            .as_bytes()
            .try_into()
            .map_err(|_| FilterParseError::InvalidTag)?;
        if !tag.iter().all(u8::is_ascii_alphanumeric) {
            return Err(FilterParseError::InvalidTag);
        }
        let raw_value = expression[operator_index + operator.len()..].trim();
        if raw_value.is_empty() {
            return Err(FilterParseError::MissingValue);
        }
        let value = if let Ok(value) = raw_value.parse::<i64>() {
            FilterValue::Integer(value)
        } else if let Ok(value) = raw_value.parse::<f64>() {
            FilterValue::Float(value)
        } else {
            FilterValue::String(
                raw_value
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .unwrap_or(raw_value)
                    .to_owned(),
            )
        };
        Ok(Self {
            tag,
            comparison,
            value: Some(value),
        })
    }
}

enum FilterScalar<'a> {
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    String(&'a str),
    Character(u8),
}

impl FilterScalar<'_> {
    fn from_value(value: &AuxValue) -> Option<FilterScalar<'_>> {
        match value {
            AuxValue::Character(value) => Some(FilterScalar::Character(*value)),
            AuxValue::Signed(value) => Some(FilterScalar::Signed(*value)),
            AuxValue::Unsigned(value) => Some(FilterScalar::Unsigned(*value)),
            AuxValue::Float(value) => Some(FilterScalar::Float(f64::from(*value))),
            AuxValue::Double(value) => Some(FilterScalar::Float(*value)),
            AuxValue::String(value) | AuxValue::HexByteArray(value) => {
                Some(FilterScalar::String(value))
            }
            AuxValue::SignedArray(_) | AuxValue::UnsignedArray(_) | AuxValue::FloatArray(_) => None,
        }
    }

    fn equal(&self, expected: &FilterValue) -> bool {
        match (self, expected) {
            (Self::Signed(actual), FilterValue::Integer(expected)) => actual == expected,
            (Self::Unsigned(actual), FilterValue::Integer(expected)) => {
                u64::try_from(*expected).is_ok_and(|expected| *actual == expected)
            }
            (Self::Float(actual), FilterValue::Float(expected)) => {
                actual.to_bits() == expected.to_bits()
            }
            (Self::Float(actual), FilterValue::Integer(expected)) => {
                actual.to_bits() == (*expected as f64).to_bits()
            }
            (Self::String(actual), FilterValue::String(expected)) => *actual == expected,
            (Self::Character(actual), FilterValue::String(expected)) => {
                expected.as_bytes() == [*actual]
            }
            _ => false,
        }
    }

    fn order(&self, expected: &FilterValue) -> Option<std::cmp::Ordering> {
        match (self, expected) {
            (Self::Signed(actual), FilterValue::Integer(expected)) => actual.partial_cmp(expected),
            (Self::Unsigned(actual), FilterValue::Integer(expected)) => {
                actual.partial_cmp(&u64::try_from(*expected).ok()?)
            }
            (Self::Float(actual), FilterValue::Float(expected)) => actual.partial_cmp(expected),
            (Self::Float(actual), FilterValue::Integer(expected)) => {
                actual.partial_cmp(&(*expected as f64))
            }
            (Self::String(actual), FilterValue::String(expected)) => {
                actual.partial_cmp(&expected.as_str())
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Fnv64(u64);

impl Fnv64 {
    fn write(&mut self, bytes: &[u8]) {
        if self.0 == 0 {
            self.0 = 0xcbf2_9ce4_8422_2325;
        }
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    const fn finish(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum FilterParseError {
    #[error("auxiliary tag must contain exactly two ASCII alphanumeric characters")]
    InvalidTag,
    #[error("auxiliary predicate has no comparison operator")]
    MissingOperator,
    #[error("auxiliary predicate has no comparison value")]
    MissingValue,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_predicates() {
        let hp: AuxPredicate = "HP>=2".parse().unwrap();
        let rg: AuxPredicate = "RG=\"tumour\"".parse().unwrap();
        let exists: AuxPredicate = "MM".parse().unwrap();
        assert_eq!(hp.tag, *b"HP");
        assert_eq!(hp.comparison, AuxComparison::GreaterOrEqual);
        assert_eq!(hp.value, Some(FilterValue::Integer(2)));
        assert_eq!(rg.value, Some(FilterValue::String("tumour".to_owned())));
        assert_eq!(exists.comparison, AuxComparison::Exists);
    }

    #[test]
    fn fingerprint_changes_with_filter_semantics() {
        let mut first = AlignmentFilter::default();
        let mut second = AlignmentFilter::default();
        first.auxiliary.push("HP=1".parse().unwrap());
        second.auxiliary.push("HP=2".parse().unwrap());
        assert_ne!(first.fingerprint(), second.fingerprint());
    }
}
