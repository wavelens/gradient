/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

let
  lock = builtins.fromJSON (builtins.readFile ../../flake.lock);
  lib = import (builtins.fetchTree lock.nodes.nixpkgs.locked + "/lib");

  eval = lib.evalModules {
    modules = [ ../../nix/modules/gradient.nix { _module.check = false; } ];
    specialArgs.pkgs = { };
  };

  entry = option: {
    name = lib.removePrefix "services.gradient." option.name;
    type = option.type;
    default = option.default.text or null;
    description = option.description or "";
  };
in map entry (lib.filter
  (option: option.visible && !option.internal && lib.hasPrefix "services.gradient." option.name)
  (lib.optionAttrSetToDocList eval.options))
