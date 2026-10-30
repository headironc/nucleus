//! The fixed two-byte vocabulary shared by input and authenticated storage.

use std::fmt;

use serde::Deserialize;
use serde_with::SerializeDisplay;
use strum::{AsRefStr, Display, EnumString, VariantArray};
use zeroize::Zeroizing;

#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    SerializeDisplay,
    AsRefStr,
    Display,
    EnumString,
    VariantArray,
)]
#[serde(try_from = "Zeroizing<String>")]
pub enum Code {
    #[strum(serialize = "01")]
    CommunistParty,
    #[strum(serialize = "02")]
    ProbationaryParty,
    #[strum(serialize = "03")]
    YouthLeague,
    #[strum(serialize = "04")]
    RevolutionaryCommittee,
    #[strum(serialize = "05")]
    DemocraticLeague,
    #[strum(serialize = "06")]
    NationalConstruction,
    #[strum(serialize = "07")]
    PromotingDemocracy,
    #[strum(serialize = "08")]
    PeasantsWorkers,
    #[strum(serialize = "09")]
    ZhiGong,
    #[strum(serialize = "10")]
    Jiusan,
    #[strum(serialize = "11")]
    TaiwanLeague,
    #[strum(serialize = "12")]
    IndependentDemocrat,
    #[strum(serialize = "13")]
    Unaffiliated,
}

impl Code {
    /// The complete fixed vocabulary in ascending two-digit code order.
    pub fn all() -> &'static [Self] {
        Self::VARIANTS
    }

    /// The display name fixed by the project's affiliation vocabulary.
    pub fn name(self) -> &'static str {
        match self {
            Self::CommunistParty => "中共党员",
            Self::ProbationaryParty => "中共预备党员",
            Self::YouthLeague => "共青团员",
            Self::RevolutionaryCommittee => "民革会员",
            Self::DemocraticLeague => "民盟盟员",
            Self::NationalConstruction => "民建会员",
            Self::PromotingDemocracy => "民进会员",
            Self::PeasantsWorkers => "农工党党员",
            Self::ZhiGong => "致公党党员",
            Self::Jiusan => "九三学社社员",
            Self::TaiwanLeague => "台盟盟员",
            Self::IndependentDemocrat => "无党派民主人士",
            Self::Unaffiliated => "群众",
        }
    }
}

impl TryFrom<Zeroizing<String>> for Code {
    type Error = strum::ParseError;

    fn try_from(value: Zeroizing<String>) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Code([redacted])")
    }
}

impl utoipa::PartialSchema for Code {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::schema::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .enum_values(Some(Self::VARIANTS.iter().map(AsRef::as_ref)))
            .into()
    }
}

impl utoipa::ToSchema for Code {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("affiliation.Code")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::Code;

    #[test]
    fn fixed_vocabulary_preserves_codes_names_and_order() {
        // Independent expected values from docs/center/values.md.
        let expected = [
            ("01", "中共党员"),
            ("02", "中共预备党员"),
            ("03", "共青团员"),
            ("04", "民革会员"),
            ("05", "民盟盟员"),
            ("06", "民建会员"),
            ("07", "民进会员"),
            ("08", "农工党党员"),
            ("09", "致公党党员"),
            ("10", "九三学社社员"),
            ("11", "台盟盟员"),
            ("12", "无党派民主人士"),
            ("13", "群众"),
        ];
        assert_eq!(Code::all().len(), expected.len());
        for (code, (value, name)) in Code::all().iter().zip(expected) {
            assert_eq!(code.as_ref(), value);
            assert_eq!(code.name(), name);
            assert_eq!(value.parse::<Code>().expect("fixed code"), *code);
            assert_eq!(serde_json::to_value(code).expect("code JSON"), value);
        }
        for value in ["", "00", "1", "14", "未填写", "群众"] {
            assert!(value.parse::<Code>().is_err());
        }
    }
}
