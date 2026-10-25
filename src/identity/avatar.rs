//! Anonymous frozen content has independent budgets and a connection-owned transfer permit.

use axum::{
    body::Bytes,
    extract::{ConnectInfo, Query, Request, State, rejection::BytesRejection},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::{
    identity::{
        avatar::{self, UploadRejection, link},
        operation,
    },
    kernel::Error as KernelError,
    keys,
};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, Instant, timeout_at};
use utoipa::ToSchema;
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouterExt},
    routes,
};

use crate::{
    extension::Extension,
    identity::{Context, current::Deadline, request},
    json::Json,
    path::Path,
    response::{Common, Failure},
    source,
};

pub(super) fn router(context: &Context) -> OpenApiRouter<Context> {
    OpenApiRouter::new()
        .routes(routes!(read))
        .routes(routes!(replace).map(|router| {
            router.route_layer(axum::middleware::from_fn_with_state(
                context.clone(),
                crate::identity::current::authenticate_upload,
            ))
        }))
        .routes(routes!(original).map(|router| {
            router.route_layer(axum::middleware::from_fn_with_state(
                context.clone(),
                crate::identity::current::authenticate_original,
            ))
        }))
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = avatar::Current, max_properties = 6)]
pub(super) struct Current<'a> {
    revision: avatar::Revision,
    #[schema(
        format = "uri",
        pattern = r#"^https://[^/?#]+/api/v1/avatars/[0-9a-f]{64}\.webp\?[^#]+(?![\s\S])"#
    )]
    href: &'a str,
    expires_at: DateTime<Utc>,
    #[schema(pattern = r#"^image/webp(?![\s\S])"#)]
    media_type: &'static str,
    #[schema(minimum = 1, maximum = 1024)]
    width: u16,
    #[schema(minimum = 1, maximum = 1024)]
    height: u16,
}

impl<'a> From<&'a avatar::Current> for Current<'a> {
    fn from(value: &'a avatar::Current) -> Self {
        Self {
            revision: value.revision(),
            href: value.link().href(),
            expires_at: value.link().expires_at(),
            media_type: "image/webp",
            width: value.width(),
            height: value.height(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    key_id: keys::Id,
    expires_at: link::Expiration,
    signature: link::Signature,
}

struct Image;
impl utoipa::PartialSchema for Image {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::Schema> {
        utoipa::openapi::schema::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .format(Some(utoipa::openapi::schema::SchemaFormat::KnownFormat(
                utoipa::openapi::schema::KnownFormat::Binary,
            )))
            .into()
    }
}
impl utoipa::ToSchema for Image {}

/// The route below the API prefix; the nested handler sees the request path without the prefix.
pub(crate) const ROUTE: &str = "/avatars/{image}";

/// matchit captures a whole segment; the handler additionally requires the fixed `.webp` suffix.
/// The common middleware sees the full request path, including the API prefix.
pub(crate) fn path(path: &str) -> bool {
    path.strip_prefix(crate::router::PREFIX)
        .and_then(|path| path.strip_prefix("/avatars/"))
        .and_then(|path| path.strip_suffix(".webp"))
        .is_some_and(|digest| !digest.is_empty() && !digest.contains('/'))
}

#[utoipa::path(get, path = "/avatars/{image}", operation_id = "avatarImage",
    params(("image" = String, Path, description = "Exactly 64 lowercase hexadecimal digest characters followed by .webp"),
        ("key_id" = i64, Query, description = "Canonical nonnegative signing key ID"), ("expires_at" = u64, Query, description = "Canonical unsigned Unix seconds"),
        ("signature" = String, Query, description = "Canonical unpadded base64url HMAC-SHA256; exactly 43 characters")),
    responses((status = 200, description = "Exact frozen WebP bytes; Operation-Id and session proof are ignored", content_type = "image/webp", body = Image,
        headers(("Cache-Control" = String, description = "private, immutable, max-age no longer than remaining signature lifetime"))),
        (status = 400, description = "invalid_input; missing, repeated or malformed proxy client address", body = Failure),
        (status = 403, description = "permission_denied for an untrusted proxy; avatar_link_invalid for an invalid signature", body = Failure), (status = 404, description = "resource_not_found", body = Failure),
        (status = 429, description = "rate_limited", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn read(
    State(context): State<Context>,
    source: source::Source,
    Extension(ConnectInfo(peer)): Extension<ConnectInfo<source::Peer>>,
    request: Request,
) -> Response {
    if let Err(error) = context.admission.image(source.address()) {
        return request::Error::Admission(error).into_response();
    }
    let permit = match context.admission.transfer() {
        Ok(permit) => permit,
        Err(error) => return request::Error::Admission(error).into_response(),
    };
    let transmission = peer.transmission();
    let deadline = match transmission.begin(permit) {
        Ok(deadline) => deadline,
        Err(error) => {
            tracing::error!(stage="avatar_transfer", kind="unavailable", error=?error);
            return failed(
                StatusCode::SERVICE_UNAVAILABLE,
                Common::DependencyUnavailable,
            );
        }
    };
    let read_capacity = match transmission.hold_read() {
        Ok(permit) => permit,
        Err(error) => {
            tracing::error!(stage="avatar_read_capacity", error=?error);
            return failed(
                StatusCode::SERVICE_UNAVAILABLE,
                Common::DependencyUnavailable,
            );
        }
    };
    let mut response = match timeout_at(deadline, image(&context, request, read_capacity)).await {
        Ok(response) => response,
        Err(cause) => {
            tracing::error!(stage="avatar_transfer", kind="timeout", error=?cause);
            failed(
                StatusCode::SERVICE_UNAVAILABLE,
                Common::DependencyUnavailable,
            )
        }
    };
    if let Err(error) = transmission.respond() {
        tracing::error!(stage="avatar_transfer", kind="unavailable", error=?error);
        response = failed(
            StatusCode::SERVICE_UNAVAILABLE,
            Common::DependencyUnavailable,
        );
    }
    // Every registered transfer, including errors and HEAD, ends the owning connection.
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    response
}

async fn image(
    context: &Context,
    request: Request,
    capacity: std::sync::Arc<tokio::sync::OwnedSemaphorePermit>,
) -> Response {
    let Some(digest) = request
        .uri()
        .path()
        .strip_prefix("/avatars/")
        .and_then(|name| name.strip_suffix(".webp"))
    else {
        return failed(StatusCode::NOT_FOUND, Common::ResourceNotFound);
    };
    let Ok(digest) = digest.parse() else {
        return invalid();
    };
    let Ok(Query(input)) = Query::<Input>::try_from_uri(request.uri()) else {
        return invalid();
    };
    // The protocol has no characters requiring URL escaping in any query field.
    if request
        .uri()
        .query()
        .is_some_and(|query| query.contains(['%', '+']))
    {
        return invalid();
    }
    let proof = link::Proof::new(digest, input.key_id, input.expires_at, input.signature);
    let started = Instant::now();
    let image = match context.services.avatar().read(proof, capacity).await {
        Ok(image) => image,
        Err(avatar::Error::Link(link::Error::Invalid)) => return invalid(),
        Err(avatar::Error::Missing) => {
            return failed(StatusCode::NOT_FOUND, Common::ResourceNotFound);
        }
        Err(_) => {
            return failed(
                StatusCode::SERVICE_UNAVAILABLE,
                Common::DependencyUnavailable,
            );
        }
    };
    let cache = image.cache().saturating_sub(started.elapsed()).as_secs();
    let cache = match HeaderValue::from_str(&format!("private, max-age={cache}, immutable")) {
        Ok(cache) => cache,
        Err(cause) => {
            tracing::error!(stage="avatar_cache", kind="internal", error=?cause);
            return failed(StatusCode::INTERNAL_SERVER_ERROR, Common::InternalError);
        }
    };
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/webp")),
            (header::CACHE_CONTROL, cache),
        ],
        image.into_bytes(),
    )
        .into_response()
}

fn failed(status: StatusCode, code: Common) -> Response {
    (status, Json(Failure { code })).into_response()
}
fn invalid() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({"code":"avatar_link_invalid"})),
    )
        .into_response()
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = avatar::UploadCompletion)]
struct Completion {
    id: operation::Id,
    changed: bool,
    revision: avatar::Revision,
    #[schema(value_type = String, format = DateTime)]
    completed_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = avatar::UploadReceipt, max_properties = 3)]
struct Receipt<'a> {
    code: &'static str,
    completion: Completion,
    avatar: Current<'a>,
}

impl<'a> Receipt<'a> {
    fn new(completion: &avatar::UploadCompletion, avatar: &'a avatar::Current) -> Self {
        Self {
            code: "ok",
            completion: Completion {
                id: completion.id(),
                changed: completion.changed(),
                revision: completion.revision(),
                completed_at: completion.completed_at(),
            },
            avatar: avatar.into(),
        }
    }
}

const MAX_BODY: usize = 2 * 1024 * 1024;
const BODY: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[utoipa::path(put, path = "/accounts/current/avatar", operation_id = "replaceAvatar",
    description = "Upload static PNG, JPEG or WebP raw bytes for the current website account. The declared Content-Type must match the detected format. Avatar-Revision is the expected revision UUID.",
    params(("Operation-Id" = Uuid, Header, description = "Original replacement operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Current website session UUID"),
        ("Avatar-Revision" = Uuid, Header, description = "Expected current avatar revision"),
        ("Content-Type" = String, Header, description = "image/png, image/jpeg or image/webp")),
    request_body(content((Image = "image/png"), (Image = "image/jpeg"), (Image = "image/webp"))),
    responses((status = 200, description = "Original completion and decisive current avatar", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 409, description = "request_mismatch or avatar_changed", body = Failure),
        (status = 413, description = "avatar_too_large", body = Failure),
        (status = 415, description = "avatar_format_unsupported", body = Failure),
        (status = 429, description = "rate_limited", body = Failure),
        (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn replace(
    State(context): State<Context>,
    Extension(actor): Extension<domain::identity::current::Actor>,
    Extension(operation_id): Extension<operation::Id>,
    Extension(Deadline(deadline)): Extension<Deadline>,
    crate::query::Query(_): crate::query::Query<Empty>,
    request: Request,
) -> Response {
    let headers = request.headers();
    let Some(expected) = header(headers, "avatar-revision").and_then(|value| value.parse().ok())
    else {
        return failure(StatusCode::BAD_REQUEST, "invalid_input");
    };
    let Some(declared) = header(headers, "content-type").map(str::to_owned) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_input");
    };
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|value| value > MAX_BODY)
    {
        return failure(StatusCode::PAYLOAD_TOO_LARGE, "avatar_too_large");
    }
    let permit = match context.services.avatar().upload_slot() {
        Ok(permit) => permit,
        Err(avatar::Error::Busy) => return limited(Duration::from_secs(1)),
        Err(error) => {
            tracing::error!(stage = "avatar_upload", kind = "unavailable", error = ?error);
            return failure(StatusCode::SERVICE_UNAVAILABLE, "dependency_unavailable");
        }
    };
    let bytes = match body(request, deadline).await {
        Ok(bytes) => bytes,
        Err(BodyError::TooLarge) => {
            return failure(StatusCode::PAYLOAD_TOO_LARGE, "avatar_too_large");
        }
        Err(BodyError::Input) => return failure(StatusCode::BAD_REQUEST, "invalid_input"),
        Err(BodyError::Timeout) => {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "dependency_unavailable");
        }
    };
    match context
        .services
        .avatar()
        .upload(
            context.services.current(),
            actor,
            operation_id,
            expected,
            declared,
            bytes,
            permit,
            deadline,
        )
        .await
    {
        Ok(result) => (Json(Receipt::new(result.completion(), result.avatar()))).into_response(),
        Err(error) => kernel(error, operation_id),
    }
}

#[utoipa::path(get, path = "/replacements/{id}", operation_id = "avatarReplacement",
    description = "Read one original avatar replacement result for the current account, with the decisive current avatar. The path ID is the original operation; the Operation-Id header identifies only this query.",
    params(("id" = Uuid, Path, description = "Original replacement operation UUID"),
        ("Operation-Id" = Uuid, Header, description = "Query request UUID"),
        ("Session-Id" = Uuid, Header, description = "Current website session UUID")),
    responses((status = 200, description = "Original completion and decisive current avatar", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 404, description = "result_unavailable", body = Failure),
        (status = 429, description = "rate_limited", body = Failure),
        (status = 503, description = "outcome_unknown or dependency_unavailable", body = Failure)))]
async fn original(
    State(context): State<Context>,
    Path(id): Path<operation::Id>,
    Extension(actor): Extension<domain::identity::current::Actor>,
    Extension(request_id): Extension<operation::Id>,
    Extension(Deadline(deadline)): Extension<Deadline>,
    crate::query::Query(_): crate::query::Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    match body {
        Ok(bytes) if bytes.is_empty() => {}
        Ok(_) => return failure(StatusCode::BAD_REQUEST, "invalid_input"),
        Err(error) => return failure(error.status(), "invalid_input"),
    }
    match context
        .services
        .avatar()
        .original(context.services.current(), &actor, id, deadline)
        .await
    {
        Ok(avatar::OriginalView::Available { completion, avatar }) => {
            (Json(Receipt::new(&completion, &avatar))).into_response()
        }
        Ok(avatar::OriginalView::Pending) => {
            failure(StatusCode::SERVICE_UNAVAILABLE, "outcome_unknown")
        }
        Ok(avatar::OriginalView::Absent) => failure(StatusCode::NOT_FOUND, "result_unavailable"),
        Err(error) => kernel(error, request_id),
    }
}

fn header<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}

enum BodyError {
    TooLarge,
    Input,
    Timeout,
}

async fn body(request: Request, deadline: Instant) -> Result<Vec<u8>, BodyError> {
    let mut body = request.into_body();
    let deadline = std::cmp::min(Instant::now() + BODY, deadline);
    let mut bytes = Vec::new();
    loop {
        let frame = match timeout_at(deadline, body.frame()).await {
            Ok(Some(Ok(frame))) => frame,
            Ok(Some(Err(_))) | Ok(None) if bytes.is_empty() => return Err(BodyError::Input),
            Ok(None) => break,
            Ok(Some(Err(_))) => return Err(BodyError::Input),
            Err(_) => return Err(BodyError::Timeout),
        };
        if let Some(data) = frame.data_ref() {
            if bytes.len().saturating_add(data.len()) > MAX_BODY {
                return Err(BodyError::TooLarge);
            }
            bytes.extend_from_slice(data);
        }
    }
    if bytes.is_empty() {
        return Err(BodyError::Input);
    }
    Ok(bytes)
}

fn kernel(error: KernelError<UploadRejection>, operation_id: operation::Id) -> Response {
    use domain::kernel::Error;
    if let Some(fault) = error.fault() {
        tracing::error!(stage = "avatar_operation", %operation_id, error = ?fault);
    }
    match error {
        Error::Rejected(rejection) => match rejection {
            UploadRejection::TooLarge => failure(StatusCode::PAYLOAD_TOO_LARGE, "avatar_too_large"),
            UploadRejection::Unsupported => failure(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "avatar_format_unsupported",
            ),
            UploadRejection::Limited => limited(Duration::from_secs(3600)),
            UploadRejection::Changed => failure(StatusCode::CONFLICT, "avatar_changed"),
            UploadRejection::Invalid => failure(StatusCode::BAD_REQUEST, "invalid_input"),
        },
        Error::Unauthenticated => failure(StatusCode::UNAUTHORIZED, "authentication_invalid"),
        Error::Denied => failure(StatusCode::FORBIDDEN, "permission_denied"),
        Error::Mismatch => failure(StatusCode::CONFLICT, "request_mismatch"),
        Error::Busy => limited(Duration::from_secs(1)),
        Error::Unknown(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "outcome_unknown"),
        Error::Unavailable(_) | Error::Corrupt(_) => {
            failure(StatusCode::SERVICE_UNAVAILABLE, "dependency_unavailable")
        }
    }
}

fn limited(seconds: Duration) -> Response {
    let mut response = failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    if let Ok(retry) = HeaderValue::from_str(&seconds.as_secs().max(1).to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, retry);
    }
    response
}

fn failure(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({ "code": code }))).into_response()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use axum::{extract::Query, http::Uri};

    use crate::identity::avatar::Input;

    #[test]
    fn query_requires_each_canonical_proof_field_once() {
        let valid =
            "key_id=7&expires_at=1700000000&signature=GESpzzRjRM44oFYcfOfkqjFP6-e5v0h7wsxWniSzUKw";
        let uri: Uri = format!("/avatars/image.webp?{valid}")
            .parse()
            .expect("valid URI");
        let Query(input) = Query::<Input>::try_from_uri(&uri).expect("typed proof fields");
        assert_eq!(*input.key_id.as_ref(), 7);
        for query in [
            String::new(),
            valid.replace("key_id=7&", ""),
            valid.replace("expires_at=1700000000&", ""),
            "key_id=7&expires_at=1700000000".into(),
            format!("{valid}&key_id=7"),
            format!("{valid}&expires_at=1700000000"),
            format!("{valid}&signature=GESpzzRjRM44oFYcfOfkqjFP6-e5v0h7wsxWniSzUKw"),
            format!("{valid}&extra=1"),
            valid.replace("key_id=7", "key_id=07"),
            valid.replace("key_id=7", "key_id=-1"),
            valid.replace("key_id=7", "key_id=9223372036854775808"),
            valid.replace("expires_at=1700000000", "expires_at=01700000000"),
            valid.replace("expires_at=1700000000", "expires_at=18446744073709551616"),
            format!("{valid}="),
            valid.replace(
                "GESpzzRjRM44oFYcfOfkqjFP6-e5v0h7wsxWniSzUKw",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB",
            ),
        ] {
            let uri: Uri = format!("/avatars/image.webp?{query}")
                .parse()
                .expect("query URI");
            assert!(Query::<Input>::try_from_uri(&uri).is_err());
        }
    }
}
