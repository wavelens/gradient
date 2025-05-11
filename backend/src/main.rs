/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use core::init_state;
use std::sync::Arc;

#[tokio::main]
pub async fn main() -> std::io::Result<()> {
    let state = init_state().await;

    let _guard = if state.cli.report_errors {
        Some(sentry::init(
            "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1",
        ))
    } else {
        None
    };

    builder::start_builder(Arc::clone(&state)).await?;
    cache::start_cache(Arc::clone(&state)).await?;
    web::serve_web(Arc::clone(&state)).await?;

    Ok(())
}
