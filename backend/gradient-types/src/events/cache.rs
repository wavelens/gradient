/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::firehose;
use crate::ids::CacheId;
use serde::{Deserialize, Serialize};

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

/// Only a freshly uploaded NAR is announcing its signing. Backfill signing is announcing nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NarSigned {
    pub cache: CacheId,
    pub hash: String,
}
firehose!(NarSigned, "cache.nar.signed");
