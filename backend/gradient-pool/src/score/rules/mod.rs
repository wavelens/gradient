/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod builtin;
pub mod estimated_time;
pub mod fair_share;
pub mod prefer_local;
pub mod qos;
pub mod resource;

pub use builtin::{
    BuiltinDeprioritizeRule, DependencyCountRule, RealisedOutputsRule, RescoreWaitRule,
    ReserveFetchWorkersRule, WaitTimeRule,
};
pub use estimated_time::EstimatedTimeRule;
pub use fair_share::FairShareRule;
pub use prefer_local::PreferLocalBuildRule;
pub use qos::QosRule;
pub use resource::{ResourceFitRule, ResourceSaturationRule};
