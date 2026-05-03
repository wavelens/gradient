/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::input::greater_than_zero;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct LimitsArgs {
    /// Maximum size in bytes of an HTTP request body for most endpoints
    /// (default 2 MiB). Caps webhook payloads, JSON bodies, etc. so an
    /// attacker cannot exhaust server memory by sending an unbounded body.
    /// The direct-build multipart upload endpoint uses
    /// `--max-direct-build-size` instead.
    #[arg(
        long,
        env = "GRADIENT_MAX_REQUEST_SIZE",
        value_parser = greater_than_zero::<usize>,
        default_value_t = 2 * 1024 * 1024,
    )]
    pub max_request_size: usize,
    /// Maximum size in bytes of a `POST /api/v1/builds` multipart upload
    /// (default 1 GiB). Direct-build uploads carry an entire repository
    /// snapshot, so the limit is larger than `--max-request-size`.
    #[arg(
        long,
        env = "GRADIENT_MAX_DIRECT_BUILD_SIZE",
        value_parser = greater_than_zero::<usize>,
        default_value_t = 1024 * 1024 * 1024,
    )]
    pub max_direct_build_size: usize,
}

impl Default for LimitsArgs {
    fn default() -> Self {
        Self {
            max_request_size: 2 * 1024 * 1024,
            max_direct_build_size: 1024 * 1024 * 1024,
        }
    }
}
