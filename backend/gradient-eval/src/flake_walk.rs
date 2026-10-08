/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use nix_bindings::eval_cache::{AttrCursor, EvalCache};
use nix_bindings::flake::{
    FetchersSettings, FlakeReference, FlakeReferenceParseFlags, FlakeSettings, LockFlags,
    LockedFlake,
};
use nix_bindings::{Context, EvalState, Store};

use crate::ipc::{AttrError, DiscoveryShard};
use crate::strip_nix_store_prefix;
use crate::wildcard_walk::{self, WalkNode};

fn to_wire(shard: wildcard_walk::Shard) -> DiscoveryShard {
    DiscoveryShard {
        pattern: wildcard_walk::segments_to_pattern(&shard.segments),
        only: shard.only,
    }
}

pub struct FlakeWalker<'a> {
    cache: EvalCache,
    _locked: LockedFlake,
    state: &'a EvalState,
}

impl<'a> FlakeWalker<'a> {
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn open(
        ctx: &Arc<Context>,
        fetch: &FetchersSettings,
        flake: &Arc<FlakeSettings>,
        state: &'a EvalState,
        flake_ref: &str,
        overrides: &[(String, String)],
    ) -> Result<Self> {
        let locked = lock_flake(ctx, fetch, flake, state, flake_ref, overrides)?;
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

    #[tracing::instrument(level = "debug", skip_all)]
    pub fn discover(
        &self,
        wildcards: &[String],
        only: Option<&[String]>,
    ) -> Result<(Vec<String>, Vec<AttrError>)> {
        let root = self.root()?;
        let (includes, excludes) = wildcard_walk::parse_patterns(wildcards);

        Ok(wildcard_walk::discover_within(
            &root, &includes, &excludes, only,
        ))
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub fn discover_split(
        &self,
        wildcards: &[String],
        only: Option<&[String]>,
    ) -> Result<(Vec<String>, Vec<DiscoveryShard>, Vec<AttrError>)> {
        let root = self.root()?;
        let (includes, excludes) = wildcard_walk::parse_patterns(wildcards);
        let (attrs, deferred, errors) =
            wildcard_walk::discover_split(&root, &includes, &excludes, only);

        Ok((attrs, deferred.into_iter().map(to_wire).collect(), errors))
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub fn plan_shards(
        &self,
        wildcards: &[String],
    ) -> Result<(Vec<DiscoveryShard>, Vec<AttrError>)> {
        let root = self.root()?;
        let (includes, _) = wildcard_walk::parse_patterns(wildcards);
        let (shards, errors) = wildcard_walk::plan_shards(&root, &includes);

        Ok((shards.into_iter().map(to_wire).collect(), errors))
    }

    #[tracing::instrument(level = "debug", skip_all, fields(attr = attr_path))]
    pub fn resolve(&self, attr_path: &str) -> Result<(String, Vec<String>)> {
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

    /// Commits are going to the WAL without a checkpoint. Concurrent shard workers would otherwise
    /// deadlock on the WAL read-slot locks.
    pub fn commit_cache(&self) -> Result<()> {
        self.cache.commit().context("committing eval cache")
    }

    pub fn checkpoint_cache(&self) -> Result<()> {
        self.cache.checkpoint().context("checkpointing eval cache")
    }
}

#[tracing::instrument(level = "debug", skip_all)]
fn lock_flake(
    ctx: &Arc<Context>,
    fetch: &FetchersSettings,
    flake: &Arc<FlakeSettings>,
    state: &EvalState,
    flake_ref: &str,
    overrides: &[(String, String)],
) -> Result<LockedFlake> {
    let parse_flags = FlakeReferenceParseFlags::new(ctx, flake)?;
    let (reference, _frag) = FlakeReference::parse(ctx, fetch, flake, &parse_flags, flake_ref)
        .with_context(|| format!("parsing flake reference '{flake_ref}'"))?;

    let mut lock_flags = LockFlags::new(ctx, flake)?;
    for (name, ref_str) in overrides {
        let (override_ref, _) = FlakeReference::parse(ctx, fetch, flake, &parse_flags, ref_str)
            .with_context(|| format!("parsing override flake reference '{ref_str}'"))?;
        lock_flags = lock_flags
            .add_input_override(name, &override_ref)
            .with_context(|| format!("applying override-input '{name}'"))?;
    }

    LockedFlake::lock(ctx, fetch, flake, state, &lock_flags, &reference)
        .with_context(|| format!("locking flake '{flake_ref}'"))
}

pub fn fingerprint(
    ctx: &Arc<Context>,
    fetch: &FetchersSettings,
    flake: &Arc<FlakeSettings>,
    state: &EvalState,
    store: &Store,
    flake_ref: &str,
    overrides: &[(String, String)],
) -> Result<Option<String>> {
    let locked = lock_flake(ctx, fetch, flake, state, flake_ref, overrides)?;

    Ok(locked.fingerprint(store, fetch)?)
}

struct CursorNode<'a> {
    cursor: AttrCursor,
    state: &'a EvalState,
}

impl CursorNode<'_> {
    fn has_attr(&self, name: &str) -> Result<bool> {
        Ok(self.cursor.maybe_get_attr(name)?.is_some())
    }
}

impl WalkNode for CursorNode<'_> {
    fn child_names(&self) -> Result<Vec<String>> {
        self.cursor
            .attrs(self.state)
            .map_err(|e| anyhow!("listing attributes: {e}"))
    }

    fn child(&self, name: &str) -> Result<Option<Self>> {
        Ok(self.cursor.maybe_get_attr(name)?.map(|cursor| CursorNode {
            cursor,
            state: self.state,
        }))
    }

    fn is_derivation(&self) -> Result<bool> {
        self.cursor
            .is_derivation()
            .map_err(|e| anyhow!("is_derivation: {e}"))
    }

    fn is_opaque(&self) -> Result<bool> {
        if self.is_derivation()? {
            return Ok(false);
        }

        Ok(self.has_attr("type")? || self.has_attr("_type")?)
    }
}
