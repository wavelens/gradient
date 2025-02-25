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

    builder::start_builder(Arc::clone(&state)).await?;
    web::serve_web(Arc::clone(&state)).await?;

    Ok(())
}
