use serde_with::{DeserializeFromStr, SerializeDisplay};
use strum::{AsRefStr, Display, EnumString, VariantArray};
use tokio_postgres::{
    GenericClient,
    types::{FromSql, Type},
};

use crate::access::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, AsRefStr, EnumString)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum Scope {
    Institution,
    Workforce,
}

impl<'a> FromSql<'a> for Scope {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(<&str>::from_sql(kind, raw)?.parse()?)
    }

    fn accepts(kind: &Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    SerializeDisplay,
    DeserializeFromStr,
    AsRefStr,
    Display,
    EnumString,
    VariantArray,
)]
pub enum Code {
    #[strum(serialize = "institution.articles")]
    InstitutionArticles,
    #[strum(serialize = "institution.roles")]
    InstitutionRoles,
    #[strum(serialize = "institution.assignments")]
    InstitutionAssignments,
    #[strum(serialize = "institution.workforce")]
    InstitutionWorkforce,
    #[strum(serialize = "institution.accounts")]
    InstitutionAccounts,
    #[strum(serialize = "institution.corrections")]
    InstitutionCorrections,
    #[strum(serialize = "institution.affiliations.read")]
    InstitutionAffiliationsRead,
    #[strum(serialize = "institution.affiliations.correct")]
    InstitutionAffiliationsCorrect,
    #[strum(serialize = "workforce.roles")]
    WorkforceRoles,
    #[strum(serialize = "workforce.assignments")]
    WorkforceAssignments,
    #[strum(serialize = "workforce.admission")]
    WorkforceAdmission,
    #[strum(serialize = "workforce.suspension")]
    WorkforceSuspension,
    #[strum(serialize = "workforce.termination")]
    WorkforceTermination,
    #[strum(serialize = "workforce.numbers")]
    WorkforceNumbers,
    #[strum(serialize = "workforce.directory")]
    WorkforceDirectory,
    #[strum(serialize = "workforce.templates")]
    WorkforceTemplates,
    #[strum(serialize = "workforce.approval")]
    WorkforceApproval,
    #[strum(serialize = "workforce.production")]
    WorkforceProduction,
    #[strum(serialize = "workforce.delivery")]
    WorkforceDelivery,
}

impl utoipa::PartialSchema for Code {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::schema::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .enum_values(Some(Self::all().iter().map(AsRef::as_ref)))
            .into()
    }
}

impl utoipa::ToSchema for Code {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("capability.Code")
    }
}

impl Code {
    pub(crate) fn all() -> &'static [Self] {
        Self::VARIANTS
    }

    pub(crate) fn scope(self) -> Scope {
        match self {
            Self::InstitutionArticles
            | Self::InstitutionRoles
            | Self::InstitutionAssignments
            | Self::InstitutionWorkforce
            | Self::InstitutionAccounts
            | Self::InstitutionCorrections
            | Self::InstitutionAffiliationsRead
            | Self::InstitutionAffiliationsCorrect => Scope::Institution,
            Self::WorkforceRoles
            | Self::WorkforceAssignments
            | Self::WorkforceAdmission
            | Self::WorkforceSuspension
            | Self::WorkforceTermination
            | Self::WorkforceNumbers
            | Self::WorkforceDirectory
            | Self::WorkforceTemplates
            | Self::WorkforceApproval
            | Self::WorkforceProduction
            | Self::WorkforceDelivery => Scope::Workforce,
        }
    }
}

impl<'a> FromSql<'a> for Code {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(<&str>::from_sql(kind, raw)?.parse()?)
    }
    fn accepts(kind: &Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

pub(crate) async fn verify(client: &impl GenericClient) -> Result<(), Error> {
    let rows = client
        .query("SELECT id, scope FROM access.capabilities", &[])
        .await?;
    if rows.len() != Code::all().len() {
        return Err(Error::Catalog);
    }
    for row in rows {
        let code: Code = row.try_get("id")?;
        let scope: &str = row.try_get("scope")?;
        if scope != code.scope().as_ref() {
            return Err(Error::Catalog);
        }
    }
    Ok(())
}
