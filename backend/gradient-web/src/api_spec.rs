/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{ErrorResponseBody, WebError};
use crate::routes::{self, BodyLimits};
use std::collections::{BTreeMap, BTreeSet};
use utoipa::openapi::path::{Operation, PathItem};
use utoipa::openapi::response::{Response, ResponseBuilder};
use utoipa::openapi::schema::{ObjectBuilder, Schema, empty};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme};
use utoipa::openapi::{Content, OpenApi as Spec, Ref, RefOr};
use utoipa::{IntoParams, IntoResponses, Modify, OpenApi, PartialSchema, ToSchema};
use utoipa_axum::router::OpenApiRouter;

pub const BEARER: &str = "bearerAuth";
const ERROR_RESPONSE: &str = "Error";
const JSON: &str = "application/json";
#[derive(OpenApi)]
#[openapi(
    info(title = "Gradient API", description = include_str!("api.md")),
    components(schemas(ErrorResponseBody)),
    modifiers(&SharedComponents)
)]
struct ApiDoc;

struct SharedComponents;

impl Modify for SharedComponents {
    fn modify(&self, spec: &mut Spec) {
        let components = spec.components.get_or_insert_default();
        components.add_security_scheme(
            BEARER,
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
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
    let mut spec = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .nest("/api/v1", routes::auth_api(&limits))
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
