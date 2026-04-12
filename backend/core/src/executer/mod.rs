/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod path_utils;
pub mod pool;
pub mod ssh;

pub use self::pool::*;
pub use self::path_utils::{get_derivation_paths, nix_store_path, strip_nix_store_prefix};
pub use self::ssh::{connect, init_session};

/// A Nix daemon client over any transport. Generic over read/write halves.
pub type GenericDaemonClient<R, W> = harmonia_store_remote::DaemonClient<R, W>;

/// A Nix daemon client over a Unix socket (the local daemon).
pub type LocalDaemonClient =
    harmonia_store_remote::DaemonClient<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>;
