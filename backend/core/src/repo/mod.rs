/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Repository layer — pure database access structs.
//!
//! Each repo takes `&DatabaseConnection` and exposes named async methods for
//! all DB operations its aggregate owns. No side effects (webhooks, log
//! finalization, CI reporting) live here — those stay in the service layer.
//!
//! # Usage
//! ```ignore
//! let build_repo = BuildRepo::new(&state.db);
//! let build = build_repo.find(id).await?.ok_or(NotFound)?;
//! let updated = build_repo.update_status(build, BuildStatus::Completed).await?;
//! ```

pub mod build;
pub mod derivation;
pub mod eval;

pub use build::BuildRepo;
pub use derivation::DerivationRepo;
pub use eval::EvalRepo;
