//! Public widget configuration, independent of supplier credentials.

use axum::extract::State;
use serde::Serialize;
use thiserror::Error;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{identity::Context, json::Json, response};

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = PublicTurnstileSettings)]
pub struct Settings {
    site_key: String,
}

impl Settings {
    pub fn new(site_key: String) -> Result<Self, Error> {
        if site_key.is_empty()
            || site_key
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(Error);
        }
        Ok(Self { site_key })
    }
}

#[derive(Debug, Error)]
#[error("challenge.site_key must be nonempty without whitespace or control characters")]
pub struct Error;

pub(super) fn router() -> OpenApiRouter<Context> {
    OpenApiRouter::new().routes(routes!(read))
}

#[derive(Serialize, ToSchema)]
#[schema(as = PublicSettingsResponse)]
struct Response {
    #[schema(example = "ok")]
    code: &'static str,
    settings: Public,
}

#[derive(Serialize, ToSchema)]
#[schema(as = PublicSettings)]
struct Public {
    challenge: Challenge,
}

#[derive(Serialize, ToSchema)]
#[schema(as = PublicChallengeSettings)]
struct Challenge {
    turnstile: Settings,
}

#[utoipa::path(get, path = "/settings", operation_id = "readPublicSettings",
    description = "Read only the public widget site key. See docs/http/site.md.",
    params(("Operation-Id" = Uuid, Header, description = "Required UUID value for this read.")),
    responses(
        (status = 200, description = "Public widget configuration", body = Response),
        (status = 400, description = "invalid_input; invalid operation header", body = response::Failure)
    )
)]
async fn read(State(context): State<Context>) -> Json<Response> {
    Json(Response {
        code: "ok",
        settings: Public {
            challenge: Challenge {
                turnstile: context.settings.challenge().clone(),
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use crate::identity::challenge::Settings;

    #[test]
    fn validates_public_site_key_without_normalizing_it() {
        assert!(Settings::new("0x4AAAAAA-fictional".into()).is_ok());
        for value in [
            "",
            " leading",
            "trailing ",
            "has space",
            "line\nbreak",
            "\u{2003}",
            "\0",
        ] {
            assert!(Settings::new(value.into()).is_err());
        }
    }
}
