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
    pub use crate::fakes::drv_reader::FakeDrvReader;
    pub use crate::fakes::job_reporter::{RecordingJobReporter, ReportedEvent};
    pub use crate::fakes::mock_server::{MockProtoServer, MockServerConn};
    pub use crate::fakes::store_fixture::{StoreFixture, load_store};
    pub use crate::fakes::worker_store::FakeWorkerStore;
    pub use crate::fixtures::*;
    pub use crate::log_storage::NoopLogStorage;
    pub use crate::fakes::webhooks::RecordingWebhookClient;
    pub use crate::state::{test_state, test_state_recorded};
}
