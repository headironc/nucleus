//! Ordinary badge requests use the website session and current-state refresh after uncertainty.

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, Request, State, rejection::BytesRejection},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use domain::{
    identity::current::{self, Actor},
    pagination::Pagination,
    workforce::application::{self, Detail, Id, Input, List, PreviewInput, ResourceId},
};
use serde::{Deserialize, Serialize};
use tokio::{sync::OwnedSemaphorePermit, time::timeout_at};
use utoipa::ToSchema;
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouterExt},
    routes,
};

use crate::{
    extension::Extension,
    identity,
    json::Json,
    path::Path,
    query::Query,
    response::{Common, Failure},
};

pub(crate) const RESOURCE_ROUTE: &str = "/workforce/application-resources/{id}";

struct Binary;
impl utoipa::PartialSchema for Binary {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::Schema> {
        utoipa::openapi::schema::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .format(Some(utoipa::openapi::schema::SchemaFormat::KnownFormat(
                utoipa::openapi::schema::KnownFormat::Binary,
            )))
            .into()
    }
}
impl ToSchema for Binary {
    fn name() -> std::borrow::Cow<'static, str> {
        "application.Binary".into()
    }
}

pub(crate) fn router(
    service: application::Service,
    authentication: identity::Context,
    admission: identity::admission::Admission,
) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(photo))
        .routes(routes!(preview).map(|router| router.layer(DefaultBodyLimit::max(8192))))
        .routes(routes!(submit).map(|router| router.layer(DefaultBodyLimit::max(128 * 1024))))
        .routes(routes!(list))
        .routes(routes!(template))
        .route_layer(middleware::from_fn_with_state(
            authentication.clone(),
            identity::current::authenticate,
        ))
        .route_layer(middleware::from_fn(wait))
        .merge(
            OpenApiRouter::new()
                .routes(routes!(detail))
                .route_layer(middleware::from_fn_with_state(
                    authentication.clone(),
                    identity::current::authenticate,
                ))
                .route_layer(middleware::from_fn(wait))
                .route_layer(middleware::from_fn(target)),
        )
        .merge(
            OpenApiRouter::new()
                .routes(routes!(resource))
                .route_layer(middleware::from_fn_with_state(admission, transfer))
                .route_layer(middleware::from_fn_with_state(
                    authentication,
                    identity::current::authenticate,
                ))
                .route_layer(middleware::from_fn(target)),
        )
        .layer(DefaultBodyLimit::max(application::photo::INPUT_LIMIT))
        .with_state(service)
}

async fn transfer(
    State(admission): State<identity::admission::Admission>,
    Extension(ConnectInfo(peer)): Extension<ConnectInfo<crate::source::Peer>>,
    mut request: Request,
    next: Next,
) -> Response {
    let permit = match admission.transfer() {
        Ok(permit) => permit,
        Err(error) => {
            tracing::error!(stage="workforce_resource_capacity", error=?error);
            return unavailable();
        }
    };
    let transmission = peer.transmission();
    let deadline = match transmission.begin(permit) {
        Ok(deadline) => deadline,
        Err(error) => {
            tracing::error!(stage="workforce_resource_transfer", kind="unavailable", error=?error);
            return unavailable();
        }
    };
    let capacity = match transmission.hold_read() {
        Ok(capacity) => capacity,
        Err(error) => {
            tracing::error!(stage="workforce_resource_read_capacity", error=?error);
            return unavailable();
        }
    };
    request.extensions_mut().insert(capacity);
    let mut response = match timeout_at(deadline, next.run(request)).await {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(stage="workforce_resource_transfer", kind="timeout", error=?error);
            unavailable()
        }
    };
    if let Err(error) = transmission.respond() {
        tracing::error!(stage="workforce_resource_transfer", kind="unavailable", error=?error);
        response = unavailable();
    }
    // The connection retains transfer capacity after Hyper copies the response body.
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    response
}

fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(Failure {
            code: Common::DependencyUnavailable,
        }),
    )
        .into_response()
}

async fn wait(request: Request, next: Next) -> Response {
    // Preview uses POST for structured selections but has no write outcome to reconcile.
    let writing = request.method() == axum::http::Method::POST
        && !request.uri().path().ends_with("/applications/preview");
    match tokio::time::timeout(std::time::Duration::from_secs(30), next.run(request)).await {
        Ok(response) => response,
        Err(_) => {
            tracing::error!(stage = "workforce_application", kind = "timeout", writing);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Failure {
                    code: if writing {
                        Common::OutcomeUnknown
                    } else {
                        Common::DependencyUnavailable
                    },
                }),
            )
                .into_response()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Serialize, ToSchema)]
#[schema(as = application::PhotoResponse)]
struct Photo {
    code: &'static str,
    #[serde(flatten)]
    photo: application::Photo,
}

#[derive(Serialize, ToSchema)]
#[schema(as = application::PreviewResponse)]
struct Preview {
    code: &'static str,
    #[serde(flatten)]
    preview: application::Preview,
}

#[derive(Serialize, ToSchema)]
#[schema(as = application::Created)]
struct Created {
    code: &'static str,
    application: application::Application,
}

#[derive(Serialize, ToSchema)]
#[schema(as = application::ListResponse)]
struct Listed {
    code: &'static str,
    #[serde(flatten)]
    list: List,
}

#[derive(Serialize, ToSchema)]
#[schema(as = application::DetailResponse)]
struct Detailed {
    code: &'static str,
    #[serde(flatten)]
    detail: Detail,
}

#[derive(Serialize, ToSchema)]
#[schema(as = application::TemplateResponse)]
struct Template {
    code: &'static str,
    #[serde(flatten)]
    choices: application::TemplateChoices,
}

#[utoipa::path(get, path = "/workforce/application-template", operation_id = "workforceApplicationTemplate",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required diagnostic correlation only; no deduplication or recovery"), ("Session-Id" = uuid::Uuid, Header)),
    responses((status = 200, body = Template, description = "Fixed layout and pickup choices before confirmed cropping"),
        (status = 400, description = "invalid_input", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds before another attempt"))), (status = 500, description = "internal_error", body = Failure),
        (status = 401, body = Failure, description = "authentication_invalid"), (status = 403, body = Failure, description = "permission_denied"),
        (status = 503, body = Failure, description = "dependency_unavailable")))]
async fn template(
    State(service): State<application::Service>,
    Extension(actor): Extension<Actor>,
    Query(_): Query<Empty>,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = super::validate_empty(bytes) {
        return invalid(status);
    }
    respond(service.template(&actor).await, |choices| {
        Json(Template {
            code: "ok",
            choices,
        })
        .into_response()
    })
}

#[utoipa::path(post, path = "/workforce/application-photos", operation_id = "workforceApplicationPhoto",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required diagnostic correlation only"), ("Session-Id" = uuid::Uuid, Header), ("Origin" = String, Header)),
    request_body(content((Binary = "image/png"), (Binary = "image/jpeg"), (Binary = "image/webp")), description = "Confirmed static bytes; at most 8 MiB"),
    responses((status = 201, body = Photo, description = "Prepared immutable photograph"),
        (status = 400, description = "invalid_input", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds before another attempt"))), (status = 500, description = "internal_error", body = Failure),
        (status = 401, body = Failure, description = "authentication_invalid"), (status = 403, body = Failure, description = "permission_denied"),
        (status = 422, body = Failure, description = "invalid_input"), (status = 503, body = Failure, description = "dependency_unavailable or outcome_unknown; retained photo capacity exhaustion is dependency_unavailable")))]
async fn photo(
    State(service): State<application::Service>,
    Extension(actor): Extension<Actor>,
    Query(_): Query<Empty>,
    headers: HeaderMap,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) => return invalid(error.status()),
    };
    let mut types = headers.get_all(header::CONTENT_TYPE).iter();
    let media_type = match (types.next(), types.next()) {
        (Some(value), None) => match value.to_str() {
            Ok("image/png") => "image/png",
            Ok("image/jpeg") => "image/jpeg",
            Ok("image/webp") => "image/webp",
            _ => return invalid(StatusCode::UNSUPPORTED_MEDIA_TYPE),
        },
        _ => return invalid(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    };
    respond(
        service
            .prepare_photo(&actor, media_type.to_owned(), bytes.to_vec())
            .await,
        |photo| (StatusCode::CREATED, Json(Photo { code: "ok", photo })).into_response(),
    )
}

#[utoipa::path(post, path = "/workforce/applications/preview", operation_id = "workforceApplicationPreview",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required diagnostic correlation only"), ("Session-Id" = uuid::Uuid, Header), ("Origin" = String, Header)), request_body(content = PreviewInput, description = "At most 8 KiB including every selected value"),
    responses((status = 200, body = Preview, description = "Trusted actual content to confirm"),
        (status = 400, description = "invalid_input", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds before another attempt"))), (status = 500, description = "internal_error", body = Failure),
        (status = 401, body = Failure, description = "authentication_invalid"), (status = 403, body = Failure, description = "permission_denied"),
        (status = 404, body = Failure, description = "target_unavailable"),
        (status = 409, body = Failure, description = "number_required"), (status = 422, body = Failure, description = "selection_unavailable, missing_glyph or text_overflow"),
        (status = 503, body = Failure, description = "dependency_unavailable")))]
async fn preview(
    State(service): State<application::Service>,
    Extension(actor): Extension<Actor>,
    Query(_): Query<Empty>,
    Json(input): Json<PreviewInput>,
) -> Response {
    respond(service.preview(&actor, input).await, |preview| {
        Json(Preview {
            code: "ok",
            preview,
        })
        .into_response()
    })
}

#[utoipa::path(post, path = "/workforce/applications", operation_id = "workforceApplicationSubmit",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required diagnostic correlation only; reusing this value does not deduplicate submissions"), ("Session-Id" = uuid::Uuid, Header), ("Origin" = String, Header)), request_body(content = Input, description = "At most 128 KiB; includes the complete trusted snapshot and all original text lines"),
    responses((status = 201, body = Created, description = "Fixed application and audit committed together; the request is not deduplicated"),
        (status = 400, description = "invalid_input", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds before another attempt"))), (status = 500, description = "internal_error", body = Failure),
        (status = 401, body = Failure, description = "authentication_invalid"), (status = 403, body = Failure, description = "permission_denied"),
        (status = 404, body = Failure, description = "target_unavailable"),
        (status = 409, body = Failure, description = "data_changed or number_required"), (status = 422, body = Failure, description = "selection_unavailable, missing_glyph or text_overflow"),
        (status = 503, body = Failure, description = "dependency_unavailable or outcome_unknown; refresh own applications, do not automatically resubmit")))]
async fn submit(
    State(service): State<application::Service>,
    Extension(actor): Extension<Actor>,
    Query(_): Query<Empty>,
    Json(input): Json<Input>,
) -> Response {
    respond(service.submit(&actor, input).await, |application| {
        (
            StatusCode::CREATED,
            Json(Created {
                code: "ok",
                application,
            }),
        )
            .into_response()
    })
}

#[utoipa::path(get, path = "/workforce/current/applications", operation_id = "workforceCurrentApplications",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required diagnostic correlation only"), ("Session-Id" = uuid::Uuid, Header), ("page" = Option<i64>, Query, minimum = 1), ("size" = Option<i64>, Query, minimum = 1, maximum = 100)),
    responses((status = 200, body = Listed, description = "Full history for active participation; otherwise only cancellable summaries"),
        (status = 400, description = "invalid_input", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds before another attempt"))), (status = 500, description = "internal_error", body = Failure),
        (status = 401, body = Failure, description = "authentication_invalid"), (status = 403, body = Failure, description = "permission_denied"), (status = 503, body = Failure, description = "dependency_unavailable")))]
async fn list(
    State(service): State<application::Service>,
    Extension(actor): Extension<Actor>,
    Query(pagination): Query<Pagination>,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = super::validate_empty(bytes) {
        return invalid(status);
    }
    respond(service.list(&actor, pagination).await, |list| {
        Json(Listed { code: "ok", list }).into_response()
    })
}

#[utoipa::path(get, path = "/workforce/current/applications/{id}", operation_id = "workforceCurrentApplication",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required diagnostic correlation only"), ("Session-Id" = uuid::Uuid, Header), ("id" = Id, Path)),
    responses((status = 200, body = Detailed, description = "The immutable own application or its restricted summary"),
        (status = 400, description = "invalid_input", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds before another attempt"))), (status = 500, description = "internal_error", body = Failure),
        (status = 401, body = Failure, description = "authentication_invalid"), (status = 403, body = Failure, description = "permission_denied"), (status = 404, body = Failure, description = "target_unavailable"),
        (status = 503, body = Failure, description = "dependency_unavailable")))]
async fn detail(
    State(service): State<application::Service>,
    Extension(actor): Extension<Actor>,
    Extension(id): Extension<Id>,
    Query(_): Query<Empty>,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = super::validate_empty(bytes) {
        return invalid(status);
    }
    respond(service.detail(&actor, id).await, |detail| {
        Json(Detailed { code: "ok", detail }).into_response()
    })
}

#[utoipa::path(get, path = "/workforce/application-resources/{id}", operation_id = "workforceApplicationResource",
    params(("Session-Id" = uuid::Uuid, Header), ("id" = ResourceId, Path)),
    responses((status = 200, content((Binary = "image/webp"), (Binary = "font/ttf"), (Binary = "font/otf")), description = "Private immutable bytes; Operation-Id is ignored"),
        (status = 400, description = "invalid_input", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds before another attempt"))), (status = 500, description = "internal_error", body = Failure),
        (status = 401, body = Failure, description = "authentication_invalid"), (status = 403, body = Failure, description = "permission_denied"), (status = 404, body = Failure, description = "target_unavailable"),
        (status = 503, body = Failure, description = "dependency_unavailable")))]
async fn resource(
    State(service): State<application::Service>,
    Extension(actor): Extension<Actor>,
    Extension(id): Extension<ResourceId>,
    Extension(capacity): Extension<Arc<OwnedSemaphorePermit>>,
    method: Method,
    Query(_): Query<Empty>,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = super::validate_empty(bytes) {
        return invalid(status);
    }
    respond(
        service
            .resource(&actor, id, capacity, method == Method::HEAD)
            .await,
        |resource| {
            (
                [
                    (header::CONTENT_TYPE, resource.media_type),
                    (header::CONTENT_LENGTH, resource.size.to_string()),
                ],
                resource.bytes,
            )
                .into_response()
        },
    )
}

async fn target(Path(id): Path<String>, mut request: Request, next: Next) -> Response {
    if request.uri().path().contains("/application-resources/") {
        match id.parse::<ResourceId>() {
            Ok(id) => {
                request.extensions_mut().insert(id);
            }
            Err(_) => return invalid(StatusCode::BAD_REQUEST),
        }
    } else {
        match id.parse::<Id>() {
            Ok(id) => {
                request.extensions_mut().insert(id);
            }
            Err(_) => return invalid(StatusCode::BAD_REQUEST),
        }
    }
    next.run(request).await
}

fn invalid(status: StatusCode) -> Response {
    (
        status,
        Json(Failure {
            code: Common::InvalidInput,
        }),
    )
        .into_response()
}

#[derive(Serialize, ToSchema)]
#[serde(transparent)]
struct Code(&'static str);
impl crate::response::AsCode for Code {}

fn respond<T>(
    result: Result<T, application::Error>,
    project: impl FnOnce(T) -> Response,
) -> Response {
    let error = match result {
        Ok(value) => return project(value),
        Err(error) => error,
    };
    let (status, code) = match &error {
        application::Error::Identity(current::Error::Invalid) => {
            (StatusCode::UNAUTHORIZED, "authentication_invalid")
        }
        application::Error::Denied => (StatusCode::FORBIDDEN, "permission_denied"),
        application::Error::TargetUnavailable => (StatusCode::NOT_FOUND, "target_unavailable"),
        application::Error::Changed => (StatusCode::CONFLICT, "data_changed"),
        application::Error::NumberRequired => (StatusCode::CONFLICT, "number_required"),
        application::Error::SelectionUnavailable => {
            (StatusCode::UNPROCESSABLE_ENTITY, "selection_unavailable")
        }
        application::Error::MissingGlyph => (StatusCode::UNPROCESSABLE_ENTITY, "missing_glyph"),
        application::Error::TextOverflow => (StatusCode::UNPROCESSABLE_ENTITY, "text_overflow"),
        application::Error::Photo(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_input"),
        application::Error::Unknown(cause) => {
            tracing::error!(stage = "workforce_application_commit", error = ?cause);
            (StatusCode::SERVICE_UNAVAILABLE, "outcome_unknown")
        }
        application::Error::Busy => {
            return crate::response::limited(std::time::Duration::from_secs(1));
        }
        _ => {
            tracing::error!(stage = "workforce_application", error = ?error);
            (StatusCode::SERVICE_UNAVAILABLE, "dependency_unavailable")
        }
    };
    (status, Json(Failure { code: Code(code) })).into_response()
}
