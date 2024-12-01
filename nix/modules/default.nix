/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

{ inputs, ... }:
let
  inherit (inputs) nixpkgs;
  inherit (nixpkgs) lib;

  modules = builtins.attrNames (lib.filterAttrs (name: _: name != "default.nix") (builtins.readDir ./.));
in builtins.listToAttrs (map (module: {
  name =  builtins.replaceStrings [ ".nix" ] [ "" ] module;
  value = ./${module};
}) modules)
