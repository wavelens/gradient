/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod agent;
pub mod config;
pub mod devices;
pub mod guest;
pub mod hypervisor;
pub mod pool;
pub mod reattach;
pub mod state;
pub mod supervisor;
pub mod unit;
pub mod vsock;

pub use agent::{GUEST_BOOT_TIMEOUT, GuestBootError, ReadyAgent, wait_ready};
pub use config::{DeviceConfig, MountConfig, SupervisorConfig, VmConfig, check_device};
pub use devices::{DeviceCount, DeviceHold, DeviceInventory, DeviceUnit, HostProbe, SysfsProbe};
pub use guest::{Guest, PlugPlan, PluggedDevices, WORK_MOUNT_TAG, device_groups, guest_address};
pub use hypervisor::{GuestImage, HypervisorApi, PciDeviceInfo, VmInfo, vm_config};
pub use pool::{GuestPool, surplus, wanted_boots, warm_target};
pub use reattach::{ReattachPlan, ReattachedGuest, plan_reattach};
pub use state::{GuestId, GuestState, HeldDevice, StoredRelay, read_states, write_state};
pub use supervisor::{run_supervisor, usb_bus_id};
pub use unit::{GuestUnits, guest_id_of, unit_name};
pub use vsock::connect_vsock;
