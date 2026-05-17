/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod build_log;
mod helpers;
mod keys;
mod management;
mod nar;
mod narinfo;
mod narlist;
mod serve;
mod upstreams;

pub use self::build_log::log;
pub use self::keys::{get_cache_key, get_cache_public_key};
pub use self::management::{
    delete_cache, delete_cache_active, delete_cache_public, get, get_cache,
    get_cache_name_available, get_public_caches, patch_cache, post_cache_active, post_cache_public,
    put,
};
pub use self::nar::{nar, upstream_nar};
pub use self::narinfo::{gradient_cache_info, nix_cache_info, path};
pub use self::narlist::ls;
pub use self::serve::serve;
pub use self::upstreams::{
    delete_cache_upstream, get_cache_upstreams, patch_cache_upstream, put_cache_upstream,
};
