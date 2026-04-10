/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use core::types::*;
use std::sync::Arc;
use tracing::{debug, warn};

/// Symlink name used for the GC root pinning a given derivation output.
fn gcroot_name(hash: &str, package: &str) -> String {
    format!("{}-{}", hash, package)
}

pub(super) async fn create_gcroot(state: &Arc<ServerState>, hash: &str, package: &str) {
    let store_path = format!("/nix/store/{}-{}", hash, package);
    let name = gcroot_name(hash, package);
    if let Err(e) = state.nix_store.add_gcroot(name.clone(), store_path).await {
        warn!(error = %e, name = %name, "Failed to create GC root");
    } else {
        debug!(name = %name, "Created GC root");
    }
}

pub(super) async fn remove_gcroot(state: &Arc<ServerState>, hash: &str, package: &str) {
    let name = gcroot_name(hash, package);
    if let Err(e) = state.nix_store.remove_gcroot(name.clone()).await {
        warn!(error = %e, name = %name, "Failed to remove GC root");
    } else {
        debug!(name = %name, "Removed GC root");
    }
}
