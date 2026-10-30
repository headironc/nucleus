//! Shared person projections after domain authentication.

use domain::identity::person;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = person::revision::Identity)]
pub(crate) struct Identity<'a> {
    person_id: person::Id,
    revision_id: person::revision::Id,
    name: &'a str,
}

impl<'a> From<&'a person::revision::Identity> for Identity<'a> {
    fn from(identity: &'a person::revision::Identity) -> Self {
        Self {
            person_id: identity.person_id(),
            revision_id: identity.revision_id(),
            name: identity.name(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = person::number::Suffix, max_properties = 1)]
pub(super) struct IdNumber<'a> {
    #[schema(min_length = 4, max_length = 4, pattern = "^[0-9]{3}[0-9X]$")]
    pub(super) suffix: &'a str,
}
