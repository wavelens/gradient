/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use bytes::Bytes;
use harmonia_protocol::daemon::{DaemonResult, DaemonStore};
use harmonia_store_path::StorePath;
use std::future::Future;

/// Both ssh-ng:// and grpc:// transports provide these store features. The protocol of the Nix daemon cannot offer an operation for them.
pub trait StoreFeaturesBeyondDaemonProtocol: DaemonStore {
    fn read_build_log<'a>(
        &'a mut self,
        drv_path: &'a StorePath,
    ) -> impl Future<Output = DaemonResult<Option<Bytes>>> + Send + 'a;
}
