/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared test utilities for the gradient backend workspace.
//!
//! Dedupes `test_cli`, `test_state`, `NoopLogStorage`, `db_with`, and fixture
//! builders that were previously copied into every crate's `tests/common.rs`.
//!
//! ```ignore
//! use test_support::prelude::*;
//!
//! #[tokio::test]
//! async fn example() {
//!     let state = test_state(db_with(vec![[org()]]));
//!     // ...
//! }
//! ```

pub mod cli;
pub mod db;
pub mod fakes;
pub mod fixtures;
pub mod log_storage;
pub mod state;

pub mod prelude {
    pub use crate::cli::test_cli;
    pub use crate::db::db_with;
    pub use crate::fixtures::*;
    pub use crate::log_storage::NoopLogStorage;
    pub use crate::state::test_state;
}
