/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::{DateTime, NaiveDateTime};
use std::ops::RangeInclusive;
use std::sync::LazyLock;
use uuid::{uuid, Uuid};

pub const PORT_RANGE: RangeInclusive<usize> = 1..=65535;

pub static NULL_TIME: LazyLock<NaiveDateTime> =
    LazyLock::new(|| DateTime::from_timestamp(0, 0).unwrap().naive_utc());

pub const FLAKE_START: [&str; 7] = [
    "checks",
    "packages",
    "formatter",
    "legacyPackages",
    "nixosConfigurations",
    "devShells",
    "hydraJobs",
];

pub const BASE_ROLE_ADMIN_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000001");
pub const BASE_ROLE_WRITE_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000002");
pub const BASE_ROLE_VIEW_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000003");
