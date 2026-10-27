/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod config;
pub mod state;

pub use config::{DeviceConfig, MountConfig, VmConfig, check_device};
pub use state::{GuestId, GuestState, HeldDevice, StoredRelay, read_states, write_state};
