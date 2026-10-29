/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "Exec apps with the Gradient frontend development VM.";
  inputs.nixpkgs.url = "http://gradient.local/nixpkgs.tar.gz";
  outputs = { nixpkgs, ... }: let
    pkgs = import nixpkgs { system = "x86_64-linux"; };
    programs = import ./exec_apps.nix { inherit pkgs; };
  in {
    apps.x86_64-linux.ticks = {
      type = "app";
      program = "${programs.ticks}";
      passthru.gradient = { };
    };
  };
}
