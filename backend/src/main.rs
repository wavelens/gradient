/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Parser;
use gradient_core::ServerExtensions;
use gradient_types::Cli;

pub fn main() -> std::io::Result<()> {
    // rustls 0.23 is panicking on a TLS handshake without an installed provider. See issue #232.
    gradient_util::http::init_crypto_provider();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .build()
        .expect("Failed to build tokio runtime")
        .block_on(gradient_server::run(
            Cli::parse(),
            ServerExtensions::default(),
        ))
}
