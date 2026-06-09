/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod build_log;
mod helpers;
mod keys;
mod management;
pub mod members;
mod nar;
mod narinfo;
mod narlist;
mod nars;
mod proto;
mod upload;
pub mod roles;
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
pub use self::proto::cache_proto;
pub use self::nars::{
    available as nars_available, delete as nars_delete, list as nars_list, show as nars_show,
    stats as nars_stats,
};
pub use self::upload::nars_upload;
pub use self::serve::serve;
pub use self::upstreams::{
    delete_cache_upstream, get_cache_upstreams, patch_cache_upstream, put_cache_upstream,
};
