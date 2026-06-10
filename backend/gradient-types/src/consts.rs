/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ids::RoleId;
use chrono::{DateTime, NaiveDateTime};
use std::ops::RangeInclusive;
use std::sync::LazyLock;
use uuid::uuid;

pub const PORT_RANGE: RangeInclusive<usize> = 1..=65535;

pub static NULL_TIME: LazyLock<NaiveDateTime> = LazyLock::new(|| {
    DateTime::from_timestamp(0, 0)
        .unwrap_or(DateTime::UNIX_EPOCH)
        .naive_utc()
});

pub const FLAKE_START: [&str; 7] = [
    "checks",
    "packages",
    "formatter",
    "legacyPackages",
    "nixosConfigurations",
    "devShells",
    "hydraJobs",
];

pub const BASE_ROLE_ADMIN_ID: RoleId = RoleId::new(uuid!("00000000-0000-0000-0000-000000000001"));
pub const BASE_ROLE_WRITE_ID: RoleId = RoleId::new(uuid!("00000000-0000-0000-0000-000000000002"));
pub const BASE_ROLE_VIEW_ID: RoleId = RoleId::new(uuid!("00000000-0000-0000-0000-000000000003"));

pub const BASE_CACHE_ROLE_ADMIN_ID: RoleId =
    RoleId::new(uuid!("00000000-0000-0000-0000-000000000011"));
pub const BASE_CACHE_ROLE_WRITE_ID: RoleId =
    RoleId::new(uuid!("00000000-0000-0000-0000-000000000012"));
pub const BASE_CACHE_ROLE_VIEW_ID: RoleId =
    RoleId::new(uuid!("00000000-0000-0000-0000-000000000013"));
