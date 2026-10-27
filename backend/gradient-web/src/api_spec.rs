/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::endpoints::badges::BadgeStyle;
use crate::error::{ErrorResponseBody, WebError};
use crate::routes::{self, BodyLimits};
use crate::scim::ScimErrorBody;
use axum::extract::DefaultBodyLimit;
use gradient_types::{ApiId, SessionId};
use std::collections::{BTreeMap, BTreeSet};
use utoipa::openapi::path::{Operation, PathItem};
use utoipa::openapi::response::{Response, ResponseBuilder};
use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, Schema, SchemaFormat, Type, empty};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme};
use utoipa::openapi::{Content, OpenApi as Spec, Ref, RefOr};
use utoipa::{IntoParams, IntoResponses, Modify, OpenApi, PartialSchema, ToSchema};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};

pub const BEARER: &str = "bearerAuth";
pub const CACHE_BASIC: &str = "cacheBasic";
pub const METRICS: &str = "metricsAuth";
pub const SCIM: &str = "scimBearer";
const ERROR_RESPONSE: &str = "Error";
const JSON: &str = "application/json";
#[derive(OpenApi)]
#[openapi(
    info(title = "Gradient API", description = include_str!("api.md")),
    components(schemas(ErrorResponseBody, ScimErrorBody, ApiId, SessionId, BadgeStyle)),
    tags(
        (name = "general", description = "Health check and server configuration"),
        (name = "board", description = "Job Board - metric queries, live assigned jobs, workers, scoring, and expensive builds"),
        (name = "webhooks", description = "Project, cache and instance webhooks for durable events"),
        (name = "auth", description = "Authentication - login, register, OAuth/OIDC"),
        (name = "user", description = "Current user profile, API keys, and settings"),
        (name = "dashboard", description = "Personal dashboard - the viewer's task table, stat tiles and activity heatmap"),
        (name = "projects", description = "Project management, members, SSH keys, cache subscriptions"),
        (name = "teams", description = "Teams, their members, workers, grants and requests"),
        (name = "tasks", description = "Task management and evaluation triggers"),
        (name = "triggers", description = "Per-task evaluation triggers (polling, reporter_push, reporter_pull_request, time)"),
        (name = "flake-inputs", description = "Per-task flake input URL overrides applied during evaluation fetch."),
        (name = "evals", description = "Evaluation details, status, and log streaming"),
        (name = "builds", description = "Build details, logs, artefact downloads, and dependency graph"),
        (name = "runner-jobs", description = "Runner jobs - status, wait reason, attempts, phases, logs and abort"),
        (name = "build-requests", description = "Ad-hoc build requests from `gradient build` - CLI-uploaded source evaluated under a per-project build-request task. Flake inputs served over `git+ssh://` are fetched using the project's SSH key, and `input_overrides` can override remote flake inputs for the run."),
        (name = "workers", description = "Worker registration and live status (proto workers connected via WebSocket)"),
        (name = "caches", description = "Binary cache management"),
        (name = "nix-cache", description = "Nix binary cache protocol, served at the server root"),
        (name = "commits", description = "Commit records"),
        (name = "actions", description = "Per-task Actions (Send Mail, Send Web Request, Git Host Status Report)"),
        (name = "git-host-webhooks", description = "Incoming Git host webhooks (external Git hosts -> Gradient push triggers)"),
        (name = "integrations", description = "Per-project named Git host integrations (inbound HMAC secrets + outbound API credentials)"),
        (name = "admin", description = "Server-wide administrative endpoints. All routes require the caller to have the `superuser` flag."),
        (name = "scim", description = "SCIM 2.0 instance-level provisioning (Users + Groups), bearer-token authenticated, mounted at `/scim/v2` only when SCIM is enabled.")
    ),
    modifiers(&SharedComponents)
)]
struct ApiDoc;

struct SharedComponents;

impl Modify for SharedComponents {
    fn modify(&self, spec: &mut Spec) {
        let components = spec.components.get_or_insert_default();
        components.add_security_scheme(
            BEARER,
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer).bearer_format("JWT")
                    .description(Some("JWT obtained from `/auth/basic/login`, or an API key prefixed with `GRAD`\n(e.g. `Bearer GRADabcdef...`)."))
                    .build(),
            ),
        );
        components.add_security_scheme(
            CACHE_BASIC,
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Basic)
                    .description(Some(
                        "Any username; a JWT or an API key as the password. Private caches only.",
                    ))
                    .build(),
            ),
        );
        components.add_security_scheme(
            METRICS,
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some("Operator-configured bearer token loaded from `GRADIENT_METRICS_TOKEN_FILE`.\nIndependent of user authentication."))
                    .build(),
            ),
        );
        components.add_security_scheme(
            SCIM,
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some("Operator-configured SCIM provisioning token loaded from\n`GRADIENT_SCIM_TOKEN_FILE`. Presented by the IdP (Okta/Entra) on every\n`/scim/v2` request. Independent of user authentication."))
                    .build(),
            ),
        );
        components
            .responses
            .insert(ERROR_RESPONSE.to_string(), RefOr::T(error_response()));
    }
}

fn error_response() -> Response {
    ResponseBuilder::new()
        .description("Refused or failed request, with the reason in `code` and `message`")
        .content(
            JSON,
            Content::new(Some(Ref::from_schema_name(ErrorResponseBody::name()))),
        )
        .build()
}

impl IntoResponses for WebError {
    fn responses() -> BTreeMap<String, RefOr<Response>> {
        BTreeMap::from([(
            "default".to_string(),
            RefOr::Ref(Ref::from_response_name(ERROR_RESPONSE)),
        )])
    }
}

#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
pub struct ProjectPath {
    pub project: String,
}

#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
pub struct TaskPath {
    pub project: String,
    pub task: String,
}

#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
pub struct CachePath {
    pub cache: String,
}

#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
pub struct TeamPath {
    pub team: String,
}

pub(crate) fn operations_mut(spec: &mut Spec) -> impl Iterator<Item = &mut Operation> {
    spec.paths
        .paths
        .values_mut()
        .flat_map(|item: &mut PathItem| {
            [
                &mut item.get,
                &mut item.put,
                &mut item.post,
                &mut item.delete,
                &mut item.patch,
                &mut item.head,
                &mut item.options,
                &mut item.trace,
            ]
            .into_iter()
            .flatten()
        })
}

pub(crate) fn enveloped<S>(mut router: OpenApiRouter<S>) -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    for operation in operations_mut(router.get_openapi_mut()) {
        for response in operation.responses.responses.values_mut() {
            let RefOr::T(response) = response else {
                continue;
            };
            if let Some(RefOr::T(content)) = response.content.get_mut(JSON) {
                content.schema = Some(envelope(content.schema.take()));
            }
        }
    }
    router
}

fn envelope(message: Option<RefOr<Schema>>) -> RefOr<Schema> {
    ObjectBuilder::new()
        .property("error", bool::schema())
        .required("error")
        .property("message", message.unwrap_or_else(|| empty().into()))
        .required("message")
        .into()
}

pub(crate) fn secured<S>(mut router: OpenApiRouter<S>, scheme: &str) -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    for operation in operations_mut(router.get_openapi_mut()) {
        operation.security = Some(vec![SecurityRequirement::new(scheme, [] as [&str; 0])]);
    }
    router
}

pub struct Binary;

impl PartialSchema for Binary {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Binary)))
            .into()
    }
}

impl ToSchema for Binary {}

pub(crate) fn optionally_secured<S>(mut router: OpenApiRouter<S>, scheme: &str) -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    for operation in operations_mut(router.get_openapi_mut()) {
        operation.security = Some(vec![
            SecurityRequirement::default(),
            SecurityRequirement::new(scheme, [] as [&str; 0]),
        ]);
    }
    router
}

pub(crate) fn unsecured<S>(mut router: OpenApiRouter<S>) -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    for operation in operations_mut(router.get_openapi_mut()) {
        operation.security = Some(Vec::new());
    }
    router
}

pub(crate) fn limited<S>(
    (schemas, paths, method_router): UtoipaMethodRouter<S>,
    limit: usize,
) -> UtoipaMethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    (
        schemas,
        paths,
        method_router.layer(DefaultBodyLimit::max(limit)),
    )
}

pub(crate) fn wildcard<S>(
    mut router: OpenApiRouter<S>,
    (schemas, paths, method_router): UtoipaMethodRouter<S>,
) -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    let route: Vec<String> = paths.paths.keys().map(|path| wildcard_path(path)).collect();
    let spec = router.get_openapi_mut();
    spec.paths.merge(paths);
    spec.components
        .get_or_insert_default()
        .schemas
        .extend(schemas);
    route.iter().fold(router, |router, path| {
        router.route(path, method_router.clone())
    })
}

fn wildcard_path(documented: &str) -> String {
    match documented.rfind('{') {
        Some(open) => format!("{}{{*{}", &documented[..open], &documented[open + 1..]),
        None => documented.to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("operation id `{0}` names more than a single operation")]
    RepeatedOperationId(String),
    #[error("operation without an operation id")]
    MissingOperationId,
    #[error("write the spec as YAML: {0}")]
    Yaml(String),
}

pub fn api_spec() -> Result<Spec, SpecError> {
    let limits = BodyLimits::for_spec();
    let api = OpenApiRouter::new()
        .merge(routes::auth_api(&limits))
        .merge(routes::optional_api())
        .merge(routes::auth_sensitive())
        .merge(routes::webhook_routes())
        .merge(routes::open_api())
        .merge(routes::cache_upload_api(&limits));
    let mut spec = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .nest("/api/v1", api)
        .merge(routes::proto_api())
        .merge(routes::metrics_api())
        .merge(routes::cache_routes())
        .merge(routes::cache_inspect())
        .merge(routes::cache_log())
        .merge(routes::cache_proto_route())
        .nest("/scim/v2", routes::scim_routes())
        .into_openapi();

    let mut seen = BTreeSet::new();
    for operation in operations_mut(&mut spec) {
        let id = operation
            .operation_id
            .clone()
            .ok_or(SpecError::MissingOperationId)?;
        if !seen.insert(id.clone()) {
            return Err(SpecError::RepeatedOperationId(id));
        }
    }
    Ok(spec)
}

pub fn api_spec_yaml() -> Result<String, SpecError> {
    api_spec()?
        .to_yaml()
        .map_err(|error| SpecError::Yaml(error.to_string()))
}
