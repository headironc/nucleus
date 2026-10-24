//! Assemble delivered routes and their matching OpenAPI document.

use axum::{
    Router,
    body::Bytes,
    extract,
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use tower_http::{
    request_id::{MakeRequestUuid, SetRequestIdLayer},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{diagnostics, json::Json, operation, response};

pub(crate) fn new(routes: OpenApiRouter) -> Result<Router, serde_json::Error> {
    let (router, mut document) = routes.split_for_parts();
    let (openapi, schema) = OpenApiRouter::<Bytes>::new()
        .routes(routes!(openapi))
        .split_for_parts();
    document.merge(schema);
    document.info.title = "Nucleus C1".into();
    document.info.version = env!("CARGO_PKG_VERSION").into();
    // Axum supplies HEAD for GET routes and removes the response body.
    for item in document.paths.paths.values_mut() {
        if let Some(get) = &item.get {
            let mut head = get.clone();
            head.operation_id = get.operation_id.as_ref().map(|id| format!("{id}Head"));
            for response in head.responses.responses.values_mut() {
                if let utoipa::openapi::RefOr::T(response) = response {
                    response.content.clear();
                }
            }
            item.head = Some(head);
        }
    }
    // Common headers describe all current JSON responses, including rejections.
    for item in document.paths.paths.values_mut() {
        for operation in [&mut item.get, &mut item.head, &mut item.post]
            .into_iter()
            .flatten()
        {
            for response in operation.responses.responses.values_mut() {
                if let utoipa::openapi::RefOr::T(response) = response {
                    response.headers.insert(
                        "Cache-Control".into(),
                        utoipa::openapi::HeaderBuilder::new()
                            .description(Some("no-store"))
                            .build()
                            .into(),
                    );
                }
            }
        }
    }
    let document = Bytes::from(serde_json::to_vec(&document)?);
    Ok(router
        .merge(openapi.with_state(document))
        .fallback(missing)
        .method_not_allowed_fallback(method)
        .layer(middleware::from_fn(identify))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(diagnostics::request)
                .on_request(diagnostics::received)
                .on_response(diagnostics::response)
                .on_body_chunk(())
                .on_eos(())
                .on_failure(()),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(middleware::map_request(
            async |mut request: extract::Request| {
                // Every attempt receives a server-generated identity before tracing begins.
                request.headers_mut().remove("x-request-id");
                request
            },
        )))
}

#[utoipa::path(get, path = "/openapi.json", operation_id = "openapi", responses((status = 200, content_type = "application/json", description = "OpenAPI 3.1 for exactly these routes. Operation-Id is ignored.")))]
async fn openapi(extract::State(document): extract::State<Bytes>) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], document).into_response()
}

async fn identify(request: extract::Request, next: Next) -> Response {
    if matches!(*request.method(), Method::GET | Method::HEAD)
        && request.uri().path() == "/openapi.json"
    {
        return next.run(request).await;
    }
    operation::require(request, next).await.into_response()
}

async fn missing() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(response::Failure {
            code: response::Common::ResourceNotFound,
        }),
    )
        .into_response()
}

async fn method() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(response::Failure {
            code: response::Common::MethodNotAllowed,
        }),
    )
        .into_response()
}
