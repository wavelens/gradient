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

  first_task_trigger_config_by_trigger = trigger:
    builtins.head (evaluateGradientStateModuleWithState {
      tasks.web = {
        project = "acme";
        repository = "https://example.com/acme/web.git";
        created_by = "admin";
        triggers = [ trigger ];
      };
    }).services.gradient.state.tasks.web.triggers;

  throws = e: !(builtins.tryEval (builtins.deepSeq e e)).success;

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
assert (first_task_trigger_config_by_trigger { type = "reporter_pull_request"; config.branches = [ "main" "flake-update/*" ]; }).config
  == { branches = [ "main" "flake-update/*" ]; };
assert (first_task_trigger_config_by_trigger { type = "polling"; }).config == { };
assert throws (first_task_trigger_config_by_trigger { type = "reporter_pull_request"; config.branches = "*"; }).config;
assert throws (first_task_trigger_config_by_trigger { type = "reporter_push"; config.branche = [ "main" ]; }).config;
pkgs.runCommand "state-option-evaluation-check" { } "touch $out"
