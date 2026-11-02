//! Shared metadata for field/statistic editors; command values retain their JSON contracts.
use rustgis_data::FieldType;

/// The vocabulary and object keys of an existing field/statistic list parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatisticList {
    /// Summary Statistics objects with `field` and `statistic` keys; fields may repeat.
    Summary,
    /// Spatial Join objects with `field`, `rule` and an optional `delimiter`; one rule per field.
    Merge,
}

impl StatisticList {
    /// Key paired with `field` in each JSON object.
    pub fn key(self) -> &'static str {
        match self {
            Self::Summary => "statistic",
            Self::Merge => "rule",
        }
    }

    /// Recognizes the same spellings as execution and returns their canonical identity, without
    /// changing saved values. For example, `P01` and `P1` name the same summary statistic.
    pub fn canonical_name(self, name: &str) -> Option<String> {
        match self {
            Self::Summary => crate::stats::Stat::parse(name).map(|stat| stat.name()),
            Self::Merge => {
                crate::join::RULES.iter().find(|(rule, _)| rule.eq_ignore_ascii_case(name.trim())).map(|(rule, _)| (*rule).to_string())
            }
        }
    }

    /// Supported names, filtered using the same type rules as execution. Unknown input types
    /// offer all choices so a model can be configured before its upstream tool has run.
    pub fn choices(self, ty: Option<FieldType>) -> Vec<String> {
        match self {
            Self::Summary => crate::stats::NAMED
                .iter()
                .filter(|(_, stat)| ty.is_none_or(|t| t.is_numeric() || stat.fits_text()))
                .map(|(name, _)| (*name).to_string())
                .chain((1..=99).filter(|_| ty.is_none_or(|t| t.is_numeric())).map(|k| format!("P{k}")))
                .collect(),
            Self::Merge => {
                crate::join::RULES.iter().filter(|(_, rule)| ty.is_none_or(|t| rule.fits(t))).map(|(name, _)| (*name).to_string()).collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Dataset, Field, GeometryType};
    use serde_json::json;

    /// Editor identities accept execution aliases without rewriting their saved spelling.
    #[test]
    fn canonical_names_match_execution_spellings() {
        for (input, expected) in [(" P01 ", Some("P1")), ("p99", Some("P99")), ("sum", Some("SUM")), ("P0", None), ("P100", None)] {
            assert_eq!(StatisticList::Summary.canonical_name(input).as_deref(), expected);
        }
        assert_eq!(StatisticList::Merge.canonical_name(" JoIn ").as_deref(), Some("join"));
        assert_eq!(StatisticList::Merge.canonical_name("median"), None);
    }

    /// Every offered operation must execute for its field type, and rejected operations must
    /// stay out of the picker. This covers Boolean's distinct min/max restriction for merging.
    #[test]
    fn choices_match_backend_type_validation() {
        for ty in [FieldType::Integer, FieldType::Double, FieldType::Text, FieldType::Boolean] {
            let ds = Dataset::new("input", GeometryType::None, vec![Field::new("value", ty)]);
            for vocabulary in [StatisticList::Summary, StatisticList::Merge] {
                let offered = vocabulary.choices(Some(ty));
                for name in vocabulary.choices(None) {
                    let mut request = json!([{"field":"value"}]);
                    request[0][vocabulary.key()] = json!(name);
                    let accepted = match vocabulary {
                        StatisticList::Summary => crate::stats::parse_requests(&ds, Some(&request)).is_ok(),
                        StatisticList::Merge => crate::join::parse_merge(&ds, Some(&request)).is_ok(),
                    };
                    assert_eq!(offered.contains(&name), accepted, "{vocabulary:?}, {ty:?}, {name}");
                }
            }
        }
    }
}
