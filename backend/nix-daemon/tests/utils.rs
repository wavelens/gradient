// SPDX-FileCopyrightText: 2024 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

pub fn init_logging() {
    tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::TRACE)
        .try_init()
        .unwrap_or_default()
}
