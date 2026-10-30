//! Management of groups, real departments, positions, department positions, the title tree and
//! subtype rules with original completions.

use std::{borrow::Cow, marker::PhantomData};

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequest, Request, State, rejection::BytesRejection},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use domain::{
    identity::{current::Actor, operation},
    workforce::{
        directory::{self, category, department, discipline, group, position, roster, title},
        number::Subtype,
    },
};
use serde::{Deserialize, Serialize};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{
        RefOr, Schema,
        schema::{AllOfBuilder, AnyOfBuilder},
    },
};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    extension::Extension,
    identity,
    json::{self, Json},
    path::Path,
    query::Query,
    response::{Common, Failure},
    route,
};

pub(crate) fn router(
    service: directory::Service,
    authentication: identity::Context,
) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .route_layer(middleware::from_fn_with_state(
            authentication.clone(),
            identity::current::authenticate,
        ))
        .route_layer(middleware::from_fn(collection))
        .merge(
            OpenApiRouter::new()
                .routes(routes!(update, delete))
                .route_layer(middleware::from_fn_with_state(
                    authentication.clone(),
                    identity::current::authenticate,
                ))
                .route_layer(middleware::from_fn(target)),
        )
        .merge(
            OpenApiRouter::new()
                .routes(routes!(list_roster, replace_roster))
                .route_layer(middleware::from_fn_with_state(
                    authentication.clone(),
                    identity::current::authenticate,
                ))
                .route_layer(middleware::from_fn(department)),
        )
        .merge(
            OpenApiRouter::new()
                .routes(routes!(list_rules))
                .route_layer(middleware::from_fn_with_state(
                    authentication.clone(),
                    identity::current::authenticate,
                )),
        )
        .merge(
            OpenApiRouter::new()
                .routes(routes!(read_selection))
                .route_layer(middleware::from_fn_with_state(
                    authentication.clone(),
                    identity::current::authenticate,
                )),
        )
        .merge(
            OpenApiRouter::new()
                .routes(routes!(configure_rule))
                .route_layer(middleware::from_fn_with_state(
                    authentication,
                    identity::current::authenticate,
                ))
                .route_layer(middleware::from_fn(subtype)),
        )
        .layer(DefaultBodyLimit::max(8192))
        .with_state(service)
}

async fn subtype(Path(subtype): Path<Subtype>, mut request: Request, next: Next) -> Response {
    request.extensions_mut().insert(subtype);
    next.run(request).await
}

async fn department(
    Path(department_id): Path<department::Id>,
    mut request: Request,
    next: Next,
) -> Response {
    request.extensions_mut().insert(department_id);
    next.run(request).await
}

async fn collection(
    Path(collection): Path<directory::Collection>,
    mut request: Request,
    next: Next,
) -> Response {
    request.extensions_mut().insert(collection);
    next.run(request).await
}

async fn target(
    Path(target): Path<directory::Target>,
    mut request: Request,
    next: Next,
) -> Response {
    request.extensions_mut().insert(target);
    next.run(request).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Serialize, ToSchema)]
#[schema(as = directory::List)]
struct List<T> {
    code: &'static str,
    #[schema(inline)]
    items: Vec<T>,
}

#[derive(Serialize, ToSchema)]
#[serde(untagged)]
#[schema(as = directory::Result)]
enum ResultBody<T> {
    Present { item: T },
    Deleted { deleted: bool },
}

#[derive(Serialize, ToSchema)]
#[schema(as = directory::Receipt)]
struct Receipt<T> {
    code: &'static str,
    #[serde(flatten)]
    result: ResultBody<T>,
}

// The path selects the schema. Several collections have identical JSON shapes,
// so an exclusive oneOf would reject valid bodies matching more than one schema.
struct CollectionBody<G, D, P, C, S, T>(PhantomData<(G, D, P, C, S, T)>);

type CreationBody = CollectionBody<
    group::Creation,
    department::Creation,
    position::Creation,
    category::Creation,
    discipline::Creation,
    title::Creation,
>;
type UpdateBody = CollectionBody<
    directory::Update<group::Item>,
    directory::Update<department::Item>,
    directory::Update<position::Item>,
    directory::Update<category::Item>,
    directory::Update<discipline::Item>,
    directory::Update<title::Item>,
>;
type DeleteBody = CollectionBody<
    directory::Delete<group::Item>,
    directory::Delete<department::Item>,
    directory::Delete<position::Item>,
    directory::Delete<category::Item>,
    directory::Delete<discipline::Item>,
    directory::Delete<title::Item>,
>;
type ListBody = CollectionBody<
    List<group::Item>,
    List<department::Item>,
    List<position::Item>,
    List<category::Item>,
    List<discipline::Item>,
    List<title::Item>,
>;
type WriteBody = CollectionBody<
    Receipt<group::Item>,
    Receipt<department::Item>,
    Receipt<position::Item>,
    Receipt<category::Item>,
    Receipt<discipline::Item>,
    Receipt<title::Item>,
>;

impl<G: ToSchema, D: ToSchema, P: ToSchema, C: ToSchema, S: ToSchema, T: ToSchema> PartialSchema
    for CollectionBody<G, D, P, C, S, T>
{
    fn schema() -> RefOr<Schema> {
        AnyOfBuilder::new()
            .description(Some("The collection path selects the complete body schema titled groups, departments, positions, categories, disciplines or titles. Every item and identifier belongs to that collection."))
            .item(AllOfBuilder::new().title(Some("groups")).item(G::schema()))
            .item(AllOfBuilder::new().title(Some("departments")).item(D::schema()))
            .item(AllOfBuilder::new().title(Some("positions")).item(P::schema()))
            .item(AllOfBuilder::new().title(Some("categories")).item(C::schema()))
            .item(AllOfBuilder::new().title(Some("disciplines")).item(S::schema()))
            .item(AllOfBuilder::new().title(Some("titles")).item(T::schema()))
            .into()
    }
}

impl<G: ToSchema, D: ToSchema, P: ToSchema, C: ToSchema, S: ToSchema, T: ToSchema> ToSchema
    for CollectionBody<G, D, P, C, S, T>
{
    fn name() -> Cow<'static, str> {
        Cow::Owned(format!(
            "directory.CollectionBody_{}_{}_{}_{}_{}_{}",
            G::name(),
            D::name(),
            P::name(),
            C::name(),
            S::name(),
            T::name()
        ))
    }

    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        G::schemas(schemas);
        D::schemas(schemas);
        P::schemas(schemas);
        C::schemas(schemas);
        S::schemas(schemas);
        T::schemas(schemas);
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = directory::roster::List)]
struct RosterList {
    code: &'static str,
    positions: Vec<roster::Entry>,
}

#[derive(Serialize, ToSchema)]
#[serde(untagged)]
#[schema(as = directory::roster::Result)]
enum RosterResult {
    Present { positions: Vec<roster::Entry> },
    Deleted { deleted: bool },
}

#[derive(Serialize, ToSchema)]
#[schema(as = directory::roster::Receipt)]
struct RosterReceipt {
    code: &'static str,
    #[serde(flatten)]
    result: RosterResult,
}

#[derive(Serialize, ToSchema)]
#[schema(as = directory::selection::Response)]
struct SelectionResponse {
    code: &'static str,
    #[serde(flatten)]
    selection: directory::selection::Selection,
}

#[derive(Serialize, ToSchema)]
#[schema(as = directory::rule::List)]
struct RuleList {
    code: &'static str,
    #[serde(flatten)]
    configuration: directory::rule::Configuration,
}

#[derive(Serialize, ToSchema)]
#[schema(as = directory::rule::Receipt)]
struct RuleReceipt {
    code: &'static str,
    rule: directory::rule::Rule,
}

#[utoipa::path(get, path = "/workforce/directories/{collection}", operation_id = "listWorkforceDirectory",
    description = "Read the entire management collection, including disabled items, ordered by index then id. Requires a normal, unsuspended current member with workforce.directory. Current permission and items share a three-second read-only snapshot. Groups, departments, positions, categories, disciplines and titles are delivered; other collection values return 400 invalid_input. No query or body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin. No cookie is set.",
    params(("collection" = inline(directory::Collection), Path), ("Operation-Id" = uuid::Uuid, Header), ("Session-Id" = uuid::Uuid, Header)),
    responses((status = 200, description = "Complete collection selected by the path", body = ListBody),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 413, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn list(
    Extension(actor): Extension<Actor>,
    Extension(collection): Extension<directory::Collection>,
    State(service): State<directory::Service>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    match body {
        Ok(body) if body.is_empty() => {}
        Err(error) => {
            return (
                error.status(),
                Json(Failure {
                    code: Common::InvalidInput,
                }),
            )
                .into_response();
        }
        Ok(_) => return invalid_query(),
    }
    route::query(
        "directory_read",
        service.list(&actor, collection),
        |items| Json(List { code: "ok", items }).into_response(),
    )
    .await
}

#[utoipa::path(get, path = "/workforce/directories", operation_id = "readWorkforceDirectorySelection",
    description = "Read the currently selectable directory for badge applicants and managers in one three-second read-only snapshot. Requires a normal account and current unsuspended Workforce membership, but no domain permission. Enabled groups, departments with fully enabled lineage, enabled roster entries and positions for those departments, and the enabled title tree are ordered by index then id; positions are ordered by department id, entry index then entry id. Every configured rule retains its department reference and reports whether that default is currently selectable; missing subtype codes are explicit. Disabled items, enabled flags and creation operation ids are not returned. Missing referenced rows are dependency failures, not empty selections. No query or body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin. No cookie is set.",
    params(("Operation-Id" = uuid::Uuid, Header), ("Session-Id" = uuid::Uuid, Header)),
    responses((status = 200, description = "Current selectable directory and subtype rules", body = SelectionResponse),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 413, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn read_selection(
    Extension(actor): Extension<Actor>,
    State(service): State<directory::Service>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    match body {
        Ok(body) if body.is_empty() => {}
        Err(error) => {
            return (
                error.status(),
                Json(Failure {
                    code: Common::InvalidInput,
                }),
            )
                .into_response();
        }
        Ok(_) => return invalid_query(),
    }
    route::query(
        "directory_selection_read",
        service.read_selection(&actor),
        |selection| {
            Json(SelectionResponse {
                code: "ok",
                selection,
            })
            .into_response()
        },
    )
    .await
}

#[utoipa::path(get, path = "/workforce/rules", operation_id = "listWorkforceRules",
    description = "Read all configured subtype rules and the explicitly listed missing subtype codes in fixed code order. Missing rules are not hidden or replaced with a first department, group or null default. Requires a normal, unsuspended current member with workforce.directory. Permission and configuration share a three-second read-only snapshot. No query or body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin. No cookie is set.",
    params(("Operation-Id" = uuid::Uuid, Header), ("Session-Id" = uuid::Uuid, Header)),
    responses((status = 200, description = "Configured rules and visible missing subtype codes", body = RuleList),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 413, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn list_rules(
    Extension(actor): Extension<Actor>,
    State(service): State<directory::Service>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    match body {
        Ok(body) if body.is_empty() => {}
        Err(error) => {
            return (
                error.status(),
                Json(Failure {
                    code: Common::InvalidInput,
                }),
            )
                .into_response();
        }
        Ok(_) => return invalid_query(),
    }
    route::query(
        "directory_rule_read",
        service.list_rules(&actor),
        |configuration| {
            Json(RuleList {
                code: "ok",
                configuration,
            })
            .into_response()
        },
    )
    .await
}

#[utoipa::path(get, path = "/workforce/directories/departments/{id}/positions", operation_id = "listWorkforceDepartmentPositions",
    description = "Read the complete position list configured for one real department, including disabled entries and disabled positions, ordered by index then entry id. Lists are not inherited from parent departments. A missing department, or a group id in its place, returns 404 target_unavailable. Requires a normal, unsuspended current member with workforce.directory. Permission and the list share a three-second read-only snapshot. Invalid department ids return 400 invalid_input before authentication. No query or body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin. No cookie is set.",
    params(("id" = uuid::Uuid, Path), ("Operation-Id" = uuid::Uuid, Header), ("Session-Id" = uuid::Uuid, Header)),
    responses((status = 200, description = "Complete department position list", body = RosterList),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 413, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn list_roster(
    Extension(actor): Extension<Actor>,
    Extension(department_id): Extension<department::Id>,
    State(service): State<directory::Service>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    match body {
        Ok(body) if body.is_empty() => {}
        Err(error) => {
            return (
                error.status(),
                Json(Failure {
                    code: Common::InvalidInput,
                }),
            )
                .into_response();
        }
        Ok(_) => return invalid_query(),
    }
    route::query(
        "directory_roster_read",
        service.list_roster(&actor, department_id),
        |positions| {
            Json(RosterList {
                code: "ok",
                positions,
            })
            .into_response()
        },
    )
    .await
}

#[utoipa::path(put, path = "/workforce/directories/departments/{id}/positions", operation_id = "replaceWorkforceDepartmentPositions",
    description = "Compare the complete expected list and replace the department's complete position list in one transaction. expected lists every current entry with id, positionId, enabled and index; replacement lists positionId, enabled and index for every position the department keeps or adds. List order does not matter; a position or entry appearing twice returns 422 invalid_input. Retained positions keep their entry id, new positions receive one and omitted positions are removed. Department and position references never change. Disabled departments and disabled positions may be configured; later selection checks their state. An already matching replacement appends a completion without changing configuration; other stale expected lists return data_changed. A missing department or replacement position returns target_unavailable. Retained matching operations return the current list, or deleted:true when the department was later deleted, without applying again; different input for the operation returns request_mismatch. Requires a normal, unsuspended current member with workforce.directory and exact Origin. No query is accepted and no cookie is set.",
    params(("id" = uuid::Uuid, Path), ("Operation-Id" = uuid::Uuid, Header), ("Session-Id" = uuid::Uuid, Header), ("Origin" = String, Header)),
    request_body(content = roster::Replace, content_type = "application/json"),
    responses((status = 200, description = "Current complete department position list or deleted:true", body = RosterReceipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "data_changed or request_mismatch", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn replace_roster(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    Extension(department_id): Extension<department::Id>,
    State(service): State<directory::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<roster::Replace>,
) -> Response {
    route::command(
        service.replace_roster(actor, operation_id, department_id, input),
        |receipt| {
            let result = match receipt {
                roster::Receipt::Present(positions) => RosterResult::Present { positions },
                roster::Receipt::Deleted => RosterResult::Deleted { deleted: true },
            };
            Json(RosterReceipt { code: "ok", result }).into_response()
        },
    )
    .await
}

#[utoipa::path(post, path = "/workforce/directories/{collection}", operation_id = "createWorkforceDirectory",
    description = "Create a group, department, position, title category, discipline or complete title. Explicit name, enabled and index are required; departments also require groupId and accept an absent or null parentId for a top-level department; disciplines require a real categoryId and titles a real disciplineId. Names are NFC, trimmed, 1-64 Unicode scalars without controls, case-sensitive and unique across their applicable name scope (groups and departments share one scope; positions have an independent second scope; categories, disciplines and titles share a third scope), including disabled items. Requires current workforce.directory and exact Origin. No query is accepted. Matching Operation-Id returns the current original item or deleted:true without recreating it. Other input for a retained operation returns request_mismatch. No cookie is set.",
    params(("collection" = inline(directory::Collection), Path), ("Operation-Id" = uuid::Uuid, Header), ("Session-Id" = uuid::Uuid, Header), ("Origin" = String, Header)),
    request_body(content = CreationBody, content_type = "application/json"),
    responses((status = 201, description = "Current original item from the path collection or deleted:true", body = WriteBody),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "name_in_use or request_mismatch", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input or invalid_parent", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn create(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    Extension(collection): Extension<directory::Collection>,
    State(service): State<directory::Service>,
    Query(_): Query<Empty>,
    request: Request,
) -> Result<Response, json::Error> {
    let input = match collection {
        directory::Collection::Groups => directory::Creation::Group(
            Json::<group::Creation>::from_request(request, &service)
                .await?
                .0,
        ),
        directory::Collection::Departments => directory::Creation::Department(
            Json::<department::Creation>::from_request(request, &service)
                .await?
                .0,
        ),
        directory::Collection::Positions => directory::Creation::Position(
            Json::<position::Creation>::from_request(request, &service)
                .await?
                .0,
        ),
        directory::Collection::Categories => directory::Creation::Category(
            Json::<category::Creation>::from_request(request, &service)
                .await?
                .0,
        ),
        directory::Collection::Disciplines => directory::Creation::Discipline(
            Json::<discipline::Creation>::from_request(request, &service)
                .await?
                .0,
        ),
        directory::Collection::Titles => directory::Creation::Title(
            Json::<title::Creation>::from_request(request, &service)
                .await?
                .0,
        ),
    };
    Ok(write(
        StatusCode::CREATED,
        service.create(actor, operation_id, collection, input),
    )
    .await)
}

#[utoipa::path(put, path = "/workforce/directories/{collection}/{id}", operation_id = "updateWorkforceDirectory",
    description = "Compare the complete expected item and replace its mutable fields; id and creation operationId must remain unchanged. A department move changes the whole subtree atomically, with same-group parents and no cycles. A discipline may move to another real category and a title to another real discipline; a missing parent returns target_unavailable. An already matching replacement completes without changing configuration; other stale input returns data_changed. Retained matching operations return the current item or deleted:true without applying again. Requires current workforce.directory and exact Origin. No query is accepted and no cookie is set.",
    params(("collection" = inline(directory::Collection), Path), ("id" = uuid::Uuid, Path), ("Operation-Id" = uuid::Uuid, Header), ("Session-Id" = uuid::Uuid, Header), ("Origin" = String, Header)),
    request_body(content = UpdateBody, content_type = "application/json"),
    responses((status = 200, description = "Current item from the path collection or deleted:true", body = WriteBody),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "name_in_use, data_changed or request_mismatch", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input or invalid_parent", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn update(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    Extension(target): Extension<directory::Target>,
    State(service): State<directory::Service>,
    Query(_): Query<Empty>,
    request: Request,
) -> Result<Response, json::Error> {
    let input = match target {
        directory::Target::Group(_) => {
            Json::<directory::Update<group::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
        directory::Target::Department(_) => {
            Json::<directory::Update<department::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
        directory::Target::Position(_) => {
            Json::<directory::Update<position::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
        directory::Target::Category(_) => {
            Json::<directory::Update<category::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
        directory::Target::Discipline(_) => {
            Json::<directory::Update<discipline::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
        directory::Target::Title(_) => {
            Json::<directory::Update<title::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
    };
    Ok(write(
        StatusCode::OK,
        service.update(actor, operation_id, target, input),
    )
    .await)
}

#[utoipa::path(delete, path = "/workforce/directories/{collection}/{id}", operation_id = "deleteWorkforceDirectory",
    description = "Delete an unreferenced item without children after comparing its complete expected value. A new request for an already absent item returns 404 target_unavailable; retained matching operations replay their completion, including deleted:true. Requires a normal, unsuspended current member with workforce.directory and exact Origin. No query is accepted and no cookie is set.",
    params(("collection" = inline(directory::Collection), Path), ("id" = uuid::Uuid, Path), ("Operation-Id" = uuid::Uuid, Header), ("Session-Id" = uuid::Uuid, Header), ("Origin" = String, Header)),
    request_body(content = DeleteBody, content_type = "application/json"),
    responses((status = 200, description = "deleted:true", body = WriteBody),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "item_in_use, data_changed or request_mismatch", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn delete(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    Extension(target): Extension<directory::Target>,
    State(service): State<directory::Service>,
    Query(_): Query<Empty>,
    request: Request,
) -> Result<Response, json::Error> {
    let input = match target {
        directory::Target::Group(_) => {
            Json::<directory::Delete<group::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
        directory::Target::Department(_) => {
            Json::<directory::Delete<department::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
        directory::Target::Position(_) => {
            Json::<directory::Delete<position::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
        directory::Target::Category(_) => {
            Json::<directory::Delete<category::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
        directory::Target::Discipline(_) => {
            Json::<directory::Delete<discipline::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
        directory::Target::Title(_) => {
            Json::<directory::Delete<title::Item>>::from_request(request, &service)
                .await?
                .0
                .into_items()
        }
    };
    Ok(write(
        StatusCode::OK,
        service.delete(actor, operation_id, target, input),
    )
    .await)
}

#[utoipa::path(put, path = "/workforce/rules/{subtype}", operation_id = "configureWorkforceRule",
    description = "Create a missing subtype rule with departmentId and position, returning 201, or supply complete expected and replacement rules to update it, returning 200. Both expected and replacement ids must equal the path subtype. An already matching replacement appends a completion without changing the rule; other stale expected values return data_changed. A creation for an existing rule returns data_changed; an update for a missing rule returns target_unavailable. Every new configuration, including an already matching replacement, requires the real department, every ancestor and its group to be enabled, otherwise invalid_department is returned. Later department edits retain the reference. Matching original operations replay their original status and current rule without rechecking selectability; different action, target or canonical input returns request_mismatch. Unknown subtype paths return 400 invalid_input before authentication. Requires a normal, unsuspended current member with workforce.directory and exact Origin. No query is accepted and no cookie is set.",
    params(("subtype" = Subtype, Path), ("Operation-Id" = uuid::Uuid, Header), ("Session-Id" = uuid::Uuid, Header), ("Origin" = String, Header)),
    request_body(content = directory::rule::Input, content_type = "application/json"),
    responses((status = 201, description = "Created original rule", body = RuleReceipt),
        (status = 200, description = "Updated original rule", body = RuleReceipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "data_changed or request_mismatch", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input or invalid_department", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn configure_rule(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    Extension(subtype): Extension<Subtype>,
    State(service): State<directory::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<directory::rule::Input>,
) -> Response {
    route::command(
        service.configure_rule(actor, operation_id, subtype, input),
        |receipt| {
            let status = if receipt.created() {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            (
                status,
                Json(RuleReceipt {
                    code: "ok",
                    rule: receipt.rule().clone(),
                }),
            )
                .into_response()
        },
    )
    .await
}

async fn write(
    status: StatusCode,
    operation: impl std::future::Future<Output = Result<directory::Receipt, directory::Error>>,
) -> Response {
    route::command(operation, |receipt| {
        let result = match receipt {
            directory::Receipt::Present(item) => ResultBody::Present { item },
            directory::Receipt::Deleted => ResultBody::Deleted { deleted: true },
        };
        (status, Json(Receipt { code: "ok", result })).into_response()
    })
    .await
}

fn invalid_query() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(Failure {
            code: Common::InvalidInput,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use serde_json::{Value, json};
    use utoipa::OpenApi;

    use super::*;

    fn resolve<'a>(document: &'a Value, reference: &Value) -> &'a Value {
        let path = reference["$ref"].as_str().expect("schema reference");
        document
            .pointer(path.strip_prefix('#').expect("local schema"))
            .expect("registered schema")
    }

    #[test]
    fn openapi_bodies_bind_each_collection_to_its_own_items_and_identifiers() {
        #[derive(OpenApi)]
        #[openapi(paths(list, create, update, delete))]
        struct Document;

        let document = serde_json::to_value(Document::openapi()).expect("OpenAPI JSON");
        for (method, path, status) in [
            ("get", "/workforce/directories/{collection}", "200"),
            ("post", "/workforce/directories/{collection}", "201"),
            ("put", "/workforce/directories/{collection}/{id}", "200"),
            ("delete", "/workforce/directories/{collection}/{id}", "200"),
        ] {
            let operation = &document["paths"][path][method];
            if method != "get" {
                let body = resolve(
                    &document,
                    &operation["requestBody"]["content"]["application/json"]["schema"],
                );
                assert!(
                    body.get("oneOf").is_none(),
                    "identical shapes must not be exclusive"
                );
                let variants = body["anyOf"].as_array().expect("collection bodies");
                assert_eq!(variants.len(), 6);
                for (index, (collection, owner)) in [
                    ("groups", "group"),
                    ("departments", "department"),
                    ("positions", "position"),
                    ("categories", "category"),
                    ("disciplines", "discipline"),
                    ("titles", "title"),
                ]
                .into_iter()
                .enumerate()
                {
                    assert_eq!(variants[index]["title"], collection);
                    let input = &variants[index]["allOf"][0];
                    assert_eq!(input["additionalProperties"], false);
                    if method == "post" {
                        for field in ["name", "enabled", "index"] {
                            assert!(
                                input["required"]
                                    .as_array()
                                    .expect("creation fields")
                                    .contains(&json!(field))
                            );
                        }
                        assert_eq!(
                            input["properties"].get("groupId").is_some(),
                            owner == "department"
                        );
                        assert_eq!(
                            input["properties"].get("categoryId").is_some(),
                            owner == "discipline"
                        );
                        assert_eq!(
                            input["properties"].get("disciplineId").is_some(),
                            owner == "title"
                        );
                    } else {
                        assert_eq!(
                            input["properties"]["expected"]["properties"]["id"]["$ref"],
                            format!("#/components/schemas/directory.{owner}.Id")
                        );
                        if method == "put" {
                            assert_eq!(
                                input["properties"]["replacement"],
                                input["properties"]["expected"]
                            );
                        }
                    }
                }
            }
            let body = resolve(
                &document,
                &operation["responses"][status]["content"]["application/json"]["schema"],
            );
            assert!(body.get("oneOf").is_none());
            let variants = body["anyOf"].as_array().expect("collection responses");
            assert_eq!(variants.len(), 6);
            for (index, owner) in [
                "group",
                "department",
                "position",
                "category",
                "discipline",
                "title",
            ]
            .into_iter()
            .enumerate()
            {
                let response = &variants[index]["allOf"][0];
                let item = if method == "get" {
                    &response["properties"]["items"]["items"]
                } else {
                    let result = resolve(&document, &response["allOf"][0]);
                    &result["oneOf"][0]["properties"]["item"]
                };
                assert_eq!(
                    item["properties"]["id"]["$ref"],
                    format!("#/components/schemas/directory.{owner}.Id")
                );
                assert_eq!(
                    resolve(&document, &item["properties"]["id"])["format"],
                    "uuid"
                );
            }
        }
    }
}
