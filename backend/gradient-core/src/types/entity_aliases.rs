/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Single-letter prefix aliases for sea-orm entity types.
//!
//! Mapping:
//! - `E*` → `gradient_entity::*::Entity` - the type carrying `find()`, `insert()`, etc.
//! - `M*` → `gradient_entity::*::Model` - a fully-loaded row.
//! - `A*` → `gradient_entity::*::ActiveModel` - for inserts/updates.
//! - `C*` → `gradient_entity::*::Column` - column references for filters.
//!
//! These aliases are pervasive in older code; new code may prefer the
//! canonical `gradient_entity::api::Entity` form, which is what sea-orm tutorials use
//! and what ripgrep on `Entity` will surface. Migrating callers is tracked
//! separately - keeping the aliases here avoids a 1000+ site mass rename.

use gradient_entity::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub id: Uuid,
    pub name: String,
    pub managed: bool,
}

pub type ListResponse = Vec<ListItem>;

pub type EAdminTask = admin_task::Entity;
pub type EApi = api::Entity;
pub type EAuditLog = audit_log::Entity;
pub type EBuild = build::Entity;
pub type EBuildProduct = build_product::Entity;
pub type EBuildRequestBlob = build_request_blob::Entity;
pub type ECache = cache::Entity;
pub type ECacheDerivation = cache_derivation::Entity;
pub type ECacheMetric = cache_metric::Entity;
pub type ECacheRole = cache_role::Entity;
pub type ECachedPath = cached_path::Entity;
pub type ECacheUpstream = cache_upstream::Entity;
pub type ECacheUser = cache_user::Entity;
pub type ECliDeviceAuthorization = cli_device_authorization::Entity;
pub type ECommit = commit::Entity;
pub type EDerivation = derivation::Entity;
pub type EDerivationDependency = derivation_dependency::Entity;
pub type EDerivationFeature = derivation_feature::Entity;
pub type EDerivationMetric = derivation_metric::Entity;
pub type EDerivationOutput = derivation_output::Entity;
pub type ECachedPathSignature = cached_path_signature::Entity;
pub type EEntryPoint = entry_point::Entity;
pub type EEntryPointMessage = entry_point_message::Entity;
pub type EEvaluation = evaluation::Entity;
pub type EEvaluationFlakeInputOverride = evaluation_flake_input_override::Entity;
pub type EEvaluationMessage = evaluation_message::Entity;
pub type EFeature = feature::Entity;
pub type EIntegration = integration::Entity;
pub type EOrganization = organization::Entity;
pub type EOrganizationCache = organization_cache::Entity;
pub type EOrganizationUser = organization_user::Entity;
pub type EProject = project::Entity;
pub type EProjectAction = project_action::Entity;
pub type EProjectActionDelivery = project_action_delivery::Entity;
pub type EProjectFlakeInputOverride = project_flake_input_override::Entity;
pub type EProjectTrigger = project_trigger::Entity;
pub type ERole = role::Entity;
pub type ESession = session::Entity;
pub type EUploadSession = upload_session::Entity;
pub type EUser = user::Entity;
pub type EWorkerRegistration = worker_registration::Entity;

pub type MAdminTask = admin_task::Model;
pub type MApi = api::Model;
pub type MAuditLog = audit_log::Model;
pub type MBuild = build::Model;
pub type MBuildProduct = build_product::Model;
pub type MBuildRequestBlob = build_request_blob::Model;
pub type MCache = cache::Model;
pub type MCacheDerivation = cache_derivation::Model;
pub type MCacheMetric = cache_metric::Model;
pub type MCacheRole = cache_role::Model;
pub type MCachedPath = cached_path::Model;
pub type MCacheUpstream = cache_upstream::Model;
pub type MCacheUser = cache_user::Model;
pub type MCliDeviceAuthorization = cli_device_authorization::Model;
pub type MCommit = commit::Model;
pub type MDerivation = derivation::Model;
pub type MDerivationDependency = derivation_dependency::Model;
pub type MDerivationFeature = derivation_feature::Model;
pub type MDerivationMetric = derivation_metric::Model;
pub type MDerivationOutput = derivation_output::Model;
pub type MCachedPathSignature = cached_path_signature::Model;
pub type MEntryPoint = entry_point::Model;
pub type MEntryPointMessage = entry_point_message::Model;
pub type MEvaluation = evaluation::Model;
pub type MEvaluationFlakeInputOverride = evaluation_flake_input_override::Model;
pub type MEvaluationMessage = evaluation_message::Model;
pub type MFeature = feature::Model;
pub type MIntegration = integration::Model;
pub type MOrganization = organization::Model;
pub type MOrganizationCache = organization_cache::Model;
pub type MOrganizationUser = organization_user::Model;
pub type MProject = project::Model;
pub type MProjectAction = project_action::Model;
pub type MProjectActionDelivery = project_action_delivery::Model;
pub type MProjectFlakeInputOverride = project_flake_input_override::Model;
pub type MProjectTrigger = project_trigger::Model;
pub type MRole = role::Model;
pub type MSession = session::Model;
pub type MUploadSession = upload_session::Model;
pub type MUser = user::Model;
pub type MWorkerRegistration = worker_registration::Model;

pub type AAdminTask = admin_task::ActiveModel;
pub type AApi = api::ActiveModel;
pub type AAuditLog = audit_log::ActiveModel;
pub type ABuild = build::ActiveModel;
pub type ABuildProduct = build_product::ActiveModel;
pub type ABuildRequestBlob = build_request_blob::ActiveModel;
pub type ACache = cache::ActiveModel;
pub type ACacheDerivation = cache_derivation::ActiveModel;
pub type ACacheMetric = cache_metric::ActiveModel;
pub type ACacheRole = cache_role::ActiveModel;
pub type ACachedPath = cached_path::ActiveModel;
pub type ACacheUpstream = cache_upstream::ActiveModel;
pub type ACacheUser = cache_user::ActiveModel;
pub type ACliDeviceAuthorization = cli_device_authorization::ActiveModel;
pub type ACommit = commit::ActiveModel;
pub type ADerivation = derivation::ActiveModel;
pub type ADerivationDependency = derivation_dependency::ActiveModel;
pub type ADerivationFeature = derivation_feature::ActiveModel;
pub type ADerivationMetric = derivation_metric::ActiveModel;
pub type ADerivationOutput = derivation_output::ActiveModel;
pub type ACachedPathSignature = cached_path_signature::ActiveModel;
pub type AEntryPoint = entry_point::ActiveModel;
pub type AEntryPointMessage = entry_point_message::ActiveModel;
pub type AEvaluation = evaluation::ActiveModel;
pub type AEvaluationFlakeInputOverride = evaluation_flake_input_override::ActiveModel;
pub type AEvaluationMessage = evaluation_message::ActiveModel;
pub type AFeature = feature::ActiveModel;
pub type AIntegration = integration::ActiveModel;
pub type AOrganization = organization::ActiveModel;
pub type AOrganizationCache = organization_cache::ActiveModel;
pub type AOrganizationUser = organization_user::ActiveModel;
pub type AProject = project::ActiveModel;
pub type AProjectAction = project_action::ActiveModel;
pub type AProjectActionDelivery = project_action_delivery::ActiveModel;
pub type AProjectFlakeInputOverride = project_flake_input_override::ActiveModel;
pub type AProjectTrigger = project_trigger::ActiveModel;
pub type ARole = role::ActiveModel;
pub type ASession = session::ActiveModel;
pub type AUploadSession = upload_session::ActiveModel;
pub type AUser = user::ActiveModel;
pub type AWorkerRegistration = worker_registration::ActiveModel;

pub type CAdminTask = admin_task::Column;
pub type CApi = api::Column;
pub type CAuditLog = audit_log::Column;
pub type CBuild = build::Column;
pub type CBuildProduct = build_product::Column;
pub type CBuildRequestBlob = build_request_blob::Column;
pub type CCache = cache::Column;
pub type CCacheDerivation = cache_derivation::Column;
pub type CCacheMetric = cache_metric::Column;
pub type CCacheRole = cache_role::Column;
pub type CCachedPath = cached_path::Column;
pub type CCacheUpstream = cache_upstream::Column;
pub type CCacheUser = cache_user::Column;
pub type CCliDeviceAuthorization = cli_device_authorization::Column;
pub type CCommit = commit::Column;
pub type CDerivation = derivation::Column;
pub type CDerivationDependency = derivation_dependency::Column;
pub type CDerivationFeature = derivation_feature::Column;
pub type CDerivationMetric = derivation_metric::Column;
pub type CDerivationOutput = derivation_output::Column;
pub type CCachedPathSignature = cached_path_signature::Column;
pub type CEntryPoint = entry_point::Column;
pub type CEntryPointMessage = entry_point_message::Column;
pub type CEvaluation = evaluation::Column;
pub type CEvaluationFlakeInputOverride = evaluation_flake_input_override::Column;
pub type CEvaluationMessage = evaluation_message::Column;
pub type CFeature = feature::Column;
pub type CIntegration = integration::Column;
pub type COrganization = organization::Column;
pub type COrganizationCache = organization_cache::Column;
pub type COrganizationUser = organization_user::Column;
pub type CProject = project::Column;
pub type CProjectAction = project_action::Column;
pub type CProjectActionDelivery = project_action_delivery::Column;
pub type CProjectFlakeInputOverride = project_flake_input_override::Column;
pub type CProjectTrigger = project_trigger::Column;
pub type CRole = role::Column;
pub type CSession = session::Column;
pub type CUploadSession = upload_session::Column;
pub type CUser = user::Column;
pub type CWorkerRegistration = worker_registration::Column;

// `R*` (Relation) aliases removed - sea-orm relations are referenced via the
// `Entity::has_many` / `belongs_to` builder API rather than the `Relation`
// enum directly. The aliases were unused outside this file. If a future
// caller needs a relation type, prefer `gradient_entity::api::Relation`.
pub use admin_task::{AdminTaskKind, AdminTaskStatus};
pub use evaluation_message::MessageLevel;

/// Convenience bundle for code that needs the attempt fields (`MBuild`) and
/// the spec fields (`MDerivation`) together. Produced by joining `build` on
/// `derivation` at query time.
#[derive(Debug, Clone)]
pub struct BuildWithDerivation {
    pub build: MBuild,
    pub derivation: MDerivation,
}
