/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cursor-backed flake-output traversal replacing the retired `eval.nix`.
//!
//! A [`FlakeWalker`] locks a flake once and opens its eval cache, then serves
//! every discover/resolve call from the same warm cursor. [`CursorNode`] adapts
//! an eval-cache [`AttrCursor`] to the pure [`WalkNode`] traversal, mirroring
//! `eval.nix`'s `tryEval`/`safeGet` error tolerance: a cursor call that errors
//! is treated as absent/empty/false so one bad attribute cannot abort discovery.

use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use gradient_exec::strip_nix_store_prefix;
use nix_bindings::eval_cache::{AttrCursor, EvalCache};
use nix_bindings::flake::{
    FetchersSettings, FlakeReference, FlakeReferenceParseFlags, FlakeSettings, LockFlags,
    LockedFlake,
};
use nix_bindings::{Context, EvalState, Store};

use crate::nix::wildcard_walk::{self, WalkNode};

/// A locked flake with an open eval cache, walked via a borrowed `EvalState`.
pub(crate) struct FlakeWalker<'a> {
    cache: EvalCache,
    _locked: LockedFlake,
    state: &'a EvalState,
}

impl<'a> FlakeWalker<'a> {
    pub(crate) fn open(
        ctx: &Arc<Context>,
        fetch: &FetchersSettings,
        flake: &FlakeSettings,
        state: &'a EvalState,
        flake_ref: &str,
    ) -> Result<Self> {
        let locked = lock_flake(ctx, fetch, flake, state, flake_ref)?;
        let cache = EvalCache::open(ctx, state, &locked)?;

        Ok(FlakeWalker {
            cache,
            _locked: locked,
            state,
        })
    }

    fn root(&self) -> Result<CursorNode<'_>> {
        Ok(CursorNode {
            cursor: self.cache.root()?,
            state: self.state,
        })
    }

    pub(crate) fn discover(&self, wildcards: &[String]) -> Result<Vec<String>> {
        let root = self.root()?;

        wildcard_walk::discover_patterns(&root, wildcards)
    }

    /// Split the include patterns into disjoint sub-patterns for memory-bounded
    /// parallel discovery (one shard per first-wildcard child). Exclusions are
    /// dropped here; the caller re-attaches them to every shard so each worker's
    /// `discover` applies them.
    pub(crate) fn plan_shards(&self, wildcards: &[String]) -> Result<Vec<String>> {
        let root = self.root()?;
        let includes: Vec<Vec<String>> = wildcards
            .iter()
            .map(|w| wildcard_walk::parse_pattern(w))
            .filter(|(exclude, _)| !exclude)
            .map(|(_, segs)| segs)
            .collect();

        Ok(wildcard_walk::plan_shards(&root, &includes)?
            .iter()
            .map(|s| wildcard_walk::segments_to_pattern(s))
            .collect())
    }

    pub(crate) fn resolve(&self, attr_path: &str) -> Result<(String, Vec<String>)> {
        let (_, segs) = wildcard_walk::parse_pattern(attr_path);
        let mut cursor = self.cache.root()?;
        for seg in &segs {
            cursor = cursor
                .maybe_get_attr(seg)?
                .ok_or_else(|| anyhow!("attribute '{seg}' not found in '{attr_path}'"))?;
        }

        let drv = cursor
            .drv_path(self.state)
            .with_context(|| format!("resolving drvPath of '{attr_path}'"))?;

        Ok((strip_nix_store_prefix(&drv), vec![]))
    }

    /// Commit eval-cache entries written during this walk to the WAL (no
    /// checkpoint), so concurrent shard workers don't deadlock on the WAL
    /// read-slot locks. The writes are durable; [`Self::checkpoint_cache`]
    /// folds them into the main `.sqlite` once at end-of-eval.
    pub(crate) fn commit_cache(&self) -> Result<()> {
        self.cache.commit().context("committing eval cache")
    }

    /// Fold the WAL into the main `.sqlite` (PASSIVE checkpoint) so the
    /// fleet-share push sees the shards' writes. Never blocks: safe to call even
    /// while another evaluator of the same flake is reading the cache.
    pub(crate) fn checkpoint_cache(&self) -> Result<()> {
        self.cache.checkpoint().context("checkpointing eval cache")
    }
}

/// Parse and lock `flake_ref`, returning the [`LockedFlake`] without opening
/// its eval cache. Shared by [`FlakeWalker::open`] and [`fingerprint`].
fn lock_flake(
    ctx: &Arc<Context>,
    fetch: &FetchersSettings,
    flake: &FlakeSettings,
    state: &EvalState,
    flake_ref: &str,
) -> Result<LockedFlake> {
    let parse_flags = FlakeReferenceParseFlags::new(ctx, flake)?;
    let (reference, _frag) = FlakeReference::parse(ctx, fetch, flake, &parse_flags, flake_ref)
        .with_context(|| format!("parsing flake reference '{flake_ref}'"))?;
    let lock_flags = LockFlags::new(ctx, flake)?;

    LockedFlake::lock(ctx, fetch, flake, state, &lock_flags, &reference)
        .with_context(|| format!("locking flake '{flake_ref}'"))
}

/// Lock `flake_ref` and return its eval-cache fingerprint without opening (and
/// thus creating) the on-disk eval cache. `None` for mutable/dirty flakes.
pub(crate) fn fingerprint(
    ctx: &Arc<Context>,
    fetch: &FetchersSettings,
    flake: &FlakeSettings,
    state: &EvalState,
    store: &Store,
    flake_ref: &str,
) -> Result<Option<String>> {
    let locked = lock_flake(ctx, fetch, flake, state, flake_ref)?;

    Ok(locked.fingerprint(store, fetch)?)
}

/// An eval-cache cursor adapted to the pure [`WalkNode`] traversal.
struct CursorNode<'a> {
    cursor: AttrCursor,
    state: &'a EvalState,
}

impl CursorNode<'_> {
    fn has_attr(&self, name: &str) -> bool {
        matches!(self.cursor.maybe_get_attr(name), Ok(Some(_)))
    }
}

impl WalkNode for CursorNode<'_> {
    fn child_names(&self) -> Result<Vec<String>> {
        Ok(self.cursor.attrs(self.state).unwrap_or_default())
    }

    fn child(&self, name: &str) -> Result<Option<Self>> {
        match self.cursor.maybe_get_attr(name) {
            Ok(Some(cursor)) => Ok(Some(CursorNode {
                cursor,
                state: self.state,
            })),
            _ => Ok(None),
        }
    }

    fn is_derivation(&self) -> Result<bool> {
        Ok(self.cursor.is_derivation().unwrap_or(false))
    }

    fn is_opaque(&self) -> Result<bool> {
        // eval.nix isOpaque: a typed attrset that is NOT a derivation.
        if self.cursor.is_derivation().unwrap_or(false) {
            return Ok(false);
        }

        Ok(self.has_attr("type") || self.has_attr("_type"))
    }
}
