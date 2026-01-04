/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ inputs, pkgs, ... }:
let
  inherit (inputs) nixpkgs;
  inherit (nixpkgs) lib;

  vms = builtins.attrNames (lib.filterAttrs (_: type: type == "directory") (builtins.readDir ./.));
in builtins.listToAttrs (map (vm: {
  name = builtins.replaceStrings [ "/" ] [ "-" ] vm;
  value = let
    driver = (pkgs.testers.runNixOSTest ./${vm}).driverInteractive;
  in {
    type = "app";
    program = "${driver}/bin/nixos-test-driver";
  };
}) vms)
