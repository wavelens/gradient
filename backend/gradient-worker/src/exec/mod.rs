/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod agent_client;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "exec jobs carry channels from the next commit on")
)]
pub mod channels;
pub mod checkout;
pub mod context;
pub mod host;
pub mod inventory;
pub mod job;
pub mod reattach;
pub mod state;
pub mod watchdog;
