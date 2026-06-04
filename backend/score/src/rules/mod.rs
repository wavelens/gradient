/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod builtin;
pub mod fair_share;
pub mod prefer_local;
pub mod resource;

pub use builtin::{
    BuiltinDeprioritizeRule, DependencyCountRule, MissingNarSizeRule, MissingPathsRule,
    ReserveFetchWorkersRule, WaitTimeRule,
};
pub use fair_share::FairShareRule;
pub use prefer_local::PreferLocalBuildRule;
pub use resource::ResourceFitRule;
