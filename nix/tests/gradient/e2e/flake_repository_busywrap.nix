/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "Test Repository for Gradient Build Server";
  inputs.nixpkgs.url = "path:[nixpkgs]";
  outputs = { self, nixpkgs, ... }: let
    pkgs = import nixpkgs { system = "x86_64-linux"; };
  in {
    packages.x86_64-linux = {
      default = pkgs.hello;

      # Needs something only the upstream cache serves: busywrap is built
      # here, busybox is passed through, and only because busywrap needs it.
      busywrap = pkgs.runCommand "busywrap" { __structuredAttrs = true; } ''
        mkdir -p $out/bin
        ln -s ${pkgs.busybox}/bin/busybox $out/bin/bb
      '';
    };
  };
}
