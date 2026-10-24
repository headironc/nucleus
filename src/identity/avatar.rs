//! Anonymous frozen content has independent budgets and a connection-owned transfer permit.

use axum::{
    extract::{ConnectInfo, Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::{
    identity::avatar::{self, link},
    keys,
};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, timeout_at};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    extension::Extension,
    identity::{Context, request},
    json::Json,
    response::{Common, Failure},
    source,
};

pub(super) fn router() -> OpenApiRouter<Context> {
    OpenApiRouter::new().routes(routes!(read))
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
    #[schema(minimum = 512, maximum = 512)]
    width: u16,
    #[schema(minimum = 512, maximum = 512)]
    height: u16,
}

impl<'a> From<&'a avatar::Current> for Current<'a> {
    fn from(value: &'a avatar::Current) -> Self {
        Self {
            revision: value.revision(),
            href: value.link().href(),
            expires_at: value.link().expires_at(),
            media_type: "image/webp",
            width: 512,
            height: 512,
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
    let mut response = match timeout_at(deadline, image(&context, request)).await {
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

async fn image(context: &Context, request: Request) -> Response {
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
    let image = match context.services.avatar().read(proof).await {
        Ok(image) => image,
        Err(avatar::Error::Link(link::Error::Invalid)) => return invalid(),
        Err(avatar::Error::Missing) => {
            return failed(StatusCode::NOT_FOUND, Common::ResourceNotFound);
        }
        Err(cause) => {
            tracing::error!(stage="avatar_read", kind="unavailable", error=?cause);
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
        image.bytes(),
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
