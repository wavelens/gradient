/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

pub mod cache_fixture;
pub mod cli;
pub mod fakes;
pub mod log_storage;
pub mod state;
pub mod web;

pub mod prelude {
    pub use crate::cli::test_cli;
    pub use crate::fakes::drv_reader::FakeDrvReader;
    pub use crate::fakes::job_reporter::{RecordingJobReporter, ReportedEvent};
    pub use crate::fakes::store_fixture::{StoreFixture, load_store};
    pub use crate::fakes::worker_store::FakeWorkerStore;
    pub use crate::log_storage::{NoopLogStorage, RecordingLogStorage};
    pub use crate::state::{TestState, test_state};
    pub use gradient_wire::testing::{MockProtoServer, MockServerConn};
    pub use gradient_worker_client::testing::{Assignment, PeerSpec, ProtoPeer};
}
