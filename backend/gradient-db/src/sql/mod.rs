/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Every hand-written statement in the backend is declared with [`crate::sql!`],
//! which registers it so the plan gate can explain it against a
//! production-scale dataset in the e2e VM test. Nothing else may build a raw
//! `Statement`: `backend/clippy.toml` denies the sea-orm constructors.

pub mod budget;
pub mod macros;
pub mod param;
pub mod plan;
pub mod query;
pub mod rules;

pub use budget::{Budget, Shape, Spill, Violation};
pub use inventory;
pub use param::Param;
pub use plan::{Measured, PlanError, measure};
pub use query::{Flag, Query, Sql, Tier, registry};
pub use rules::check;
