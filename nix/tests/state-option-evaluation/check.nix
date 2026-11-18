/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, lib }:
let
  evaluateGradientStateModuleWithState = state: (lib.evalModules {
    modules = [
      "${pkgs.path}/nixos/modules/misc/assertions.nix"
      ../../modules/gradient-state.nix
      { services.gradient.state = state; }
    ];
  }).config;

  failedAssertionMessages = state:
    map (a: a.message) (lib.filter (a: !a.assertion) (evaluateGradientStateModuleWithState state).assertions);

  cacheWithUpstreams = upstream_caches: {
    caches.main = {
      created_by = "admin";
      signing_key_file = "/run/keys/cache";
      inherit upstream_caches;
    };
  };

  external = {
    type = "external";
    display_name = "cache.example.org";
    url = "https://cache.example.org";
    public_key = "cache.example.org-1:key";
  };
  internal = {
    type = "internal";
    cache_name = "other";
  };

  failsWith = state: expected:
    let messages = failedAssertionMessages state;
    in builtins.length messages == 1 && lib.all (part: lib.hasInfix part (builtins.head messages)) expected;
in
assert failedAssertionMessages (cacheWithUpstreams [ external internal ]) == [ ];
assert failedAssertionMessages { caches.main = { created_by = "admin"; signing_key_file = "/run/keys/cache"; }; } == [ ];
assert failsWith (cacheWithUpstreams [ (external // { display_name = null; }) ])
  [ "caches.main.upstream_caches.0" "`external`" "`display_name`" ];
assert failsWith (cacheWithUpstreams [ external { type = "external"; } ])
  [ "caches.main.upstream_caches.1" "`display_name`, `url`, `public_key`" ];
assert failsWith (cacheWithUpstreams [ { type = "internal"; display_name = "Other"; } ])
  [ "caches.main.upstream_caches.0" "`internal`" "`cache_name`" ];
pkgs.runCommand "state-option-evaluation-check" { } "touch $out"
