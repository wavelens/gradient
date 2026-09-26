/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Connected-worker registry, capability aggregation and the scoring rules the
//! scheduler and the proxy share.

pub mod peer_auth;
pub mod score;
pub mod session_port;
pub mod worker_caps;
pub mod worker_pool;
pub mod worker_state;

pub use self::peer_auth::PeerAuth;
pub use self::worker_caps::WorkerCaps;
pub use self::worker_pool::{WorkerInfo, WorkerPool, WorkerSlot};
pub use self::worker_state::WorkerShared;
