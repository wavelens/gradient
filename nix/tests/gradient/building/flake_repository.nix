/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
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
      build-test = { depth ? 1, width ? 1, seed ? "42" }: import ./build-test.nix { inherit self pkgs depth width seed; };
      default = self.packages.x86_64-linux.build-test { depth = 2; };
    };
  };
}
