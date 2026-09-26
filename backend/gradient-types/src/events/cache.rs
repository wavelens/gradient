/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::firehose;
use crate::ids::CacheId;
use serde::{Deserialize, Serialize};

/// Cache contents or stats changed; subscribers refetch their own scope-filtered view.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Changed {}
firehose!(Changed, "cache.changed");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NarFetched {
    pub cache: CacheId,
    pub hash: String,
    pub size: u64,
}
firehose!(NarFetched, "cache.nar.fetched");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NarinfoServed {
    pub cache: CacheId,
    pub hash: String,
    pub hit: bool,
}
firehose!(NarinfoServed, "cache.narinfo.served");
