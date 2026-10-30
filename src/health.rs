//! Probe only the capabilities currently required for application readiness.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use domain::{identity::operation, startup::Services};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{extension::Extension, json::Json, response};

pub(crate) fn router(services: Services) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(live))
        .routes(routes!(ready))
        .with_state(services)
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct Live {
    code: &'static str,
    live: bool,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct Ready {
    code: &'static str,
    ready: bool,
}

#[utoipa::path(get, path = "/health/live", operation_id = "liveness",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID value.")),
    responses(
        (status = 200, body = Live, description = "Process responds."),
        (status = 400, body = response::Failure, description = "invalid_input; invalid Operation-Id"),
        (status = 500, body = response::Failure, description = "internal_error")
    )
)]
async fn live(Extension(_): Extension<operation::Id>) -> Json<Live> {
    Json(Live {
        code: "ok",
        live: true,
    })
}

#[utoipa::path(get, path = "/health/ready", operation_id = "readiness",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID value.")),
    responses(
        (status = 200, body = Ready, description = "Trusted database time and shared request work are available."),
        (status = 400, body = response::Failure, description = "invalid_input; invalid Operation-Id"),
        (status = 500, body = response::Failure, description = "internal_error"),
        (status = 503, body = response::Failure, description = "dependency_unavailable; no supplier call")
    )
)]
async fn ready(
    Extension(operation_id): Extension<operation::Id>,
    State(services): State<Services>,
) -> Response {
    match services.ready().await {
        Ok(()) => Json(Ready {
            code: "ok",
            ready: true,
        })
        .into_response(),
        Err(error) => {
            tracing::error!(%operation_id, stage = "readiness", kind = "unavailable", error = ?error);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(response::Failure {
                    code: response::Common::DependencyUnavailable,
                }),
            )
                .into_response()
        }
    }
}
