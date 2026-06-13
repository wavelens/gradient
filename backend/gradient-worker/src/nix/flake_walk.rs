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
use nix_bindings::{Context, EvalState};

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
        let parse_flags = FlakeReferenceParseFlags::new(ctx, flake)?;
        let (reference, _frag) = FlakeReference::parse(ctx, fetch, flake, &parse_flags, flake_ref)
            .with_context(|| format!("parsing flake reference '{flake_ref}'"))?;
        let lock_flags = LockFlags::new(ctx, flake)?;
        let locked = LockedFlake::lock(ctx, fetch, flake, state, &lock_flags, &reference)
            .with_context(|| format!("locking flake '{flake_ref}'"))?;
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
