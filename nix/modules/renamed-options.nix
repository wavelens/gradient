/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, options, ... }: let
  option = path: [ "services" "gradient" ] ++ path;
  removed = path: lib.mkRemovedOptionModule (option path);

  removedStateOptions = [
    { path = [ "workers" "*" "enable_fetch" ]; reason = "replaced by enable_network and enable_secrets"; }
    { path = [ "projects" "*" "restrict_evaluation_fetches" ]; reason = "moved to services.gradient.state.tasks.<name>.restrict_evaluation_fetches"; }
    { path = [ "projects" "*" "extra_allowed_uri_prefixes" ]; reason = "moved to services.gradient.state.tasks.<name>.extra_allowed_uri_prefixes"; }
    { path = [ "tasks" "*" "sign_cache" ]; reason = "moved to services.gradient.state.caches.<name>.projects.*.sign_outputs"; }
    { path = [ "tasks" "*" "triggers" "*" "integration" ]; reason = "replaced by services.gradient.state.tasks.<name>.git_host_connection"; }
    { path = [ "integrations" ]; reason = "replaced by services.gradient.state.git_host_connections"; }
  ];

  findDefinitionPathsMatchingPattern = value: pattern:
    let
      key = lib.head pattern;
      rest = lib.tail pattern;
      prefixed = name: child: map (path: [ name ] ++ path) (findDefinitionPathsMatchingPattern child rest);
    in
    if pattern == [ ] then [ [ ] ]
    else if key == "*" && lib.isAttrs value then lib.concatLists (lib.mapAttrsToList prefixed value)
    else if key == "*" && lib.isList value then lib.concatLists (lib.imap0 (index: prefixed (toString index)) value)
    else if lib.isAttrs value && value ? ${key} then prefixed key value.${key}
    else [ ];
in {
  imports = [
    (removed [ "worker" "capabilities" "fetch" ] "replaced by services.gradient.worker.capabilities.network and services.gradient.worker.capabilities.secrets")
    (removed [ "worker" "nixBin" ] "the worker reads system features from the Nix store instead of running nix")
    (removed [ "worker" "packages" "nix" ] "the worker no longer runs nix; set nix.package for the daemon")
    (removed [ "gc" "wedgedEvalHours" ] "stuck evaluations no longer block garbage collection")
    (removed [ "nar" "verifyDigest" ] "all uploaded NAR files now pass hash verification on the server")
    (removed [ "publicStats" ] "worker list moved to admin API, superuser access only")
    (removed [ "storePath" ] "the server does not read this option anymore")
  ];

  config.assertions = lib.concatMap (def:
    lib.concatMap (r:
      map (path: let option = lib.concatStringsSep "." path; in {
        assertion = false;
        message = "Gradient removed the option `services.gradient.state.${option}' that was defined in ${def.file}: ${r.reason}.";
      }) (findDefinitionPathsMatchingPattern def.value r.path)
    ) removedStateOptions
  ) options.services.gradient.state.definitionsWithLocations;
}
