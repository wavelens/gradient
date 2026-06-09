/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::input::greater_than_zero;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct LimitsArgs {
    /// Maximum size in bytes of an HTTP request body for most endpoints (default 2 MiB).
    #[arg(
        long,
        env = "GRADIENT_MAX_REQUEST_SIZE",
        value_parser = greater_than_zero::<usize>,
        default_value_t = 2 * 1024 * 1024,
    )]
    pub max_request_size: usize,

    /// Maximum size in bytes of a NAR upload to `POST /caches/{cache}/nars` (default 512 MiB).
    #[arg(
        long,
        env = "GRADIENT_MAX_NAR_UPLOAD_SIZE",
        value_parser = greater_than_zero::<usize>,
        default_value_t = 512 * 1024 * 1024,
    )]
    pub max_nar_upload_size: usize,
}

impl Default for LimitsArgs {
    fn default() -> Self {
        Self {
            max_request_size: 2 * 1024 * 1024,
            max_nar_upload_size: 512 * 1024 * 1024,
        }
    }
}
