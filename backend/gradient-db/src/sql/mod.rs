/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nothing else may build a raw `Statement`.
//! `backend/clippy.toml` is denying the sea-orm constructors.

pub mod budget;
pub mod macros;
pub mod param;
pub mod plan;
pub mod query;
pub mod rules;
pub mod status;

pub use budget::{Budget, Shape, Spill, Violation};
pub use inventory;
pub use param::Param;
pub use plan::{Measured, PlanError, Scan, measure};
pub use query::{Flag, Query, Sql, Tier, registry};
pub use rules::check;
