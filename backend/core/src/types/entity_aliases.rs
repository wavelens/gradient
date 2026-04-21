/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use entity::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub id: Uuid,
    pub name: String,
    pub managed: bool,
}

pub type ListResponse = Vec<ListItem>;

pub type EApi = api::Entity;
pub type EBuild = build::Entity;
pub type EBuildProduct = build_product::Entity;
pub type ECache = cache::Entity;
pub type ECacheDerivation = cache_derivation::Entity;
pub type ECacheMetric = cache_metric::Entity;
pub type ECachedPath = cached_path::Entity;
pub type ECacheUpstream = cache_upstream::Entity;
pub type ECommit = commit::Entity;
pub type EDerivation = derivation::Entity;
pub type EDerivationDependency = derivation_dependency::Entity;
pub type EDerivationFeature = derivation_feature::Entity;
pub type EDerivationOutput = derivation_output::Entity;
pub type ECachedPathSignature = cached_path_signature::Entity;
pub type EDirectBuild = direct_build::Entity;
pub type EEntryPoint = entry_point::Entity;
pub type EEntryPointMessage = entry_point_message::Entity;
pub type EEvaluation = evaluation::Entity;
pub type EEvaluationMessage = evaluation_message::Entity;
pub type EFeature = feature::Entity;
pub type EIntegration = integration::Entity;
pub type EOrganization = organization::Entity;
pub type EOrganizationCache = organization_cache::Entity;
pub type EOrganizationUser = organization_user::Entity;
pub type EProject = project::Entity;
pub type EProjectIntegration = project_integration::Entity;
pub type ERole = role::Entity;
pub type EUser = user::Entity;
pub type EWebhook = webhook::Entity;
pub type EWorkerRegistration = worker_registration::Entity;

pub type MApi = api::Model;
pub type MBuild = build::Model;
pub type MBuildProduct = build_product::Model;
pub type MCache = cache::Model;
pub type MCacheDerivation = cache_derivation::Model;
pub type MCacheMetric = cache_metric::Model;
pub type MCachedPath = cached_path::Model;
pub type MCacheUpstream = cache_upstream::Model;
pub type MCommit = commit::Model;
pub type MDerivation = derivation::Model;
pub type MDerivationDependency = derivation_dependency::Model;
pub type MDerivationFeature = derivation_feature::Model;
pub type MDerivationOutput = derivation_output::Model;
pub type MCachedPathSignature = cached_path_signature::Model;
pub type MDirectBuild = direct_build::Model;
pub type MEntryPoint = entry_point::Model;
pub type MEntryPointMessage = entry_point_message::Model;
pub type MEvaluation = evaluation::Model;
pub type MEvaluationMessage = evaluation_message::Model;
pub type MFeature = feature::Model;
pub type MIntegration = integration::Model;
pub type MOrganization = organization::Model;
pub type MOrganizationCache = organization_cache::Model;
pub type MOrganizationUser = organization_user::Model;
pub type MProject = project::Model;
pub type MProjectIntegration = project_integration::Model;
pub type MRole = role::Model;
pub type MUser = user::Model;
pub type MWebhook = webhook::Model;
pub type MWorkerRegistration = worker_registration::Model;

pub type AApi = api::ActiveModel;
pub type ABuild = build::ActiveModel;
pub type ABuildProduct = build_product::ActiveModel;
pub type ACache = cache::ActiveModel;
pub type ACacheDerivation = cache_derivation::ActiveModel;
pub type ACacheMetric = cache_metric::ActiveModel;
pub type ACachedPath = cached_path::ActiveModel;
pub type ACacheUpstream = cache_upstream::ActiveModel;
pub type ACommit = commit::ActiveModel;
pub type ADerivation = derivation::ActiveModel;
pub type ADerivationDependency = derivation_dependency::ActiveModel;
pub type ADerivationFeature = derivation_feature::ActiveModel;
pub type ADerivationOutput = derivation_output::ActiveModel;
pub type ACachedPathSignature = cached_path_signature::ActiveModel;
pub type ADirectBuild = direct_build::ActiveModel;
pub type AEntryPoint = entry_point::ActiveModel;
pub type AEntryPointMessage = entry_point_message::ActiveModel;
pub type AEvaluation = evaluation::ActiveModel;
pub type AEvaluationMessage = evaluation_message::ActiveModel;
pub type AFeature = feature::ActiveModel;
pub type AIntegration = integration::ActiveModel;
pub type AOrganization = organization::ActiveModel;
pub type AOrganizationCache = organization_cache::ActiveModel;
pub type AOrganizationUser = organization_user::ActiveModel;
pub type AProject = project::ActiveModel;
pub type AProjectIntegration = project_integration::ActiveModel;
pub type ARole = role::ActiveModel;
pub type AUser = user::ActiveModel;
pub type AWebhook = webhook::ActiveModel;
pub type AWorkerRegistration = worker_registration::ActiveModel;

pub type CApi = api::Column;
pub type CBuild = build::Column;
pub type CBuildProduct = build_product::Column;
pub type CCache = cache::Column;
pub type CCacheDerivation = cache_derivation::Column;
pub type CCacheMetric = cache_metric::Column;
pub type CCachedPath = cached_path::Column;
pub type CCacheUpstream = cache_upstream::Column;
pub type CCommit = commit::Column;
pub type CDerivation = derivation::Column;
pub type CDerivationDependency = derivation_dependency::Column;
pub type CDerivationFeature = derivation_feature::Column;
pub type CDerivationOutput = derivation_output::Column;
pub type CCachedPathSignature = cached_path_signature::Column;
pub type CDirectBuild = direct_build::Column;
pub type CEntryPoint = entry_point::Column;
pub type CEntryPointMessage = entry_point_message::Column;
pub type CEvaluation = evaluation::Column;
pub type CEvaluationMessage = evaluation_message::Column;
pub type CFeature = feature::Column;
pub type CIntegration = integration::Column;
pub type COrganization = organization::Column;
pub type COrganizationCache = organization_cache::Column;
pub type COrganizationUser = organization_user::Column;
pub type CProject = project::Column;
pub type CProjectIntegration = project_integration::Column;
pub type CRole = role::Column;
pub type CUser = user::Column;
pub type CWebhook = webhook::Column;
pub type CWorkerRegistration = worker_registration::Column;

pub type RApi = api::Relation;
pub type RBuild = build::Relation;
pub type RCache = cache::Relation;
pub type RCacheDerivation = cache_derivation::Relation;
pub type RCachedPath = cached_path::Relation;
pub type RCommit = commit::Relation;
pub type RDerivation = derivation::Relation;
pub type RDerivationDependency = derivation_dependency::Relation;
pub type RDerivationFeature = derivation_feature::Relation;
pub type RDerivationOutput = derivation_output::Relation;
pub type RCachedPathSignature = cached_path_signature::Relation;
pub type RDirectBuild = direct_build::Relation;
pub type REntryPoint = entry_point::Relation;
pub type REntryPointMessage = entry_point_message::Relation;
pub type REvaluation = evaluation::Relation;
pub type REvaluationMessage = evaluation_message::Relation;
pub use evaluation_message::MessageLevel;
pub type RFeature = feature::Relation;
pub type RIntegration = integration::Relation;
pub type ROrganization = organization::Relation;
pub type ROrganizationCache = organization_cache::Relation;
pub type ROrganizationUser = organization_user::Relation;
pub type RProject = project::Relation;
pub type RProjectIntegration = project_integration::Relation;
pub type RRole = role::Relation;
pub type RUser = user::Relation;
pub type RWebhook = webhook::Relation;

/// Convenience bundle for code that needs the attempt fields (`MBuild`) and
/// the spec fields (`MDerivation`) together. Produced by joining `build` on
/// `derivation` at query time.
#[derive(Debug, Clone)]
pub struct BuildWithDerivation {
    pub build: MBuild,
    pub derivation: MDerivation,
}
