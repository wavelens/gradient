/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod addresses;
pub mod agent;
pub mod budget;
pub mod config;
pub mod console;
pub mod custom_image;
pub mod devices;
pub mod guest;
pub mod guest_store;
pub mod hypervisor;
pub mod pool;
pub mod reattach;
pub mod state;
pub mod supervisor;
pub mod unit;
pub mod vm_states;
pub mod vsock;
pub mod writable_store;

pub use addresses::{AddressLease, GuestAddresses};
pub use agent::{GUEST_BOOT_TIMEOUT, GuestBootError, ReadyAgent, wait_ready};
pub use budget::{BudgetHold, GuestBudget};
pub use config::{DeviceConfig, MountConfig, SupervisorConfig, VmConfig, check_device};
pub use console::ConsoleLink;
pub use custom_image::NO_GUEST_AGENT;
pub use devices::{DeviceCount, DeviceHold, DeviceInventory, DeviceUnit, HostProbe, SysfsProbe};
pub use guest::{Guest, PlugPlan, PluggedDevices, WORK_MOUNT_TAG, WorkVolume, device_groups};
pub use guest_store::GuestStorePassthrough;
pub use hypervisor::{GuestImage, HypervisorApi, PciDeviceInfo, VmInfo, vm_config};
pub use pool::{GuestPool, NO_GUEST_BUDGET, wanted_warm_guests};
pub use reattach::{ReattachPlan, ReattachedGuest, plan_reattach};
pub use state::{GuestId, GuestState, HeldDevice, StoredRelay, read_states, write_state};
pub use supervisor::{run_supervisor, usb_bus_id};
pub use unit::{GuestUnits, guest_id_of, unit_name};
pub use vm_states::{VmStateChange, VmStates};
pub use vsock::connect_vsock;
