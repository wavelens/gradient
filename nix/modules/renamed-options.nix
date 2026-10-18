/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, options, ... }: let
  option = path: [ "services" "gradient" ] ++ path;
  removed = path: lib.mkRemovedOptionModule (option path);

  removedStateOptions = [
    { group = "workers"; name = "enable_fetch"; reason = "replaced by enable_network and enable_secrets"; }
  ];

  stateEntries = group: value:
    let entries = if lib.isAttrs value then value.${group} or { } else { }; in
    if lib.isAttrs entries then entries else { };
in {
  imports = [
    (removed [ "worker" "capabilities" "fetch" ] "replaced by services.gradient.worker.capabilities.network and services.gradient.worker.capabilities.secrets")
  ];

  config.assertions = lib.concatMap (def:
    lib.concatMap (r:
      lib.mapAttrsToList (entry: _: {
        assertion = false;
        message = "The option `services.gradient.state.${r.group}.${entry}.${r.name}' defined in ${def.file} has been removed: ${r.reason}.";
      }) (lib.filterAttrs (_: v: lib.isAttrs v && v ? ${r.name}) (stateEntries r.group def.value))
    ) removedStateOptions
  ) options.services.gradient.state.definitionsWithLocations;
}
