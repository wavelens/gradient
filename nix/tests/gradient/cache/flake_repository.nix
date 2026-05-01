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
      # Synthetic derivation. Builder-side closure (`coreutils`, `bash`) is
      # pre-installed on the worker via `systemPackages`, and the worker's BFS
      # prunes already-substituted edges so the bootstrap inputDrvs don't get
      # walked.  The test VM has no internet, so any unsubstituted fetchurl
      # in the closure would break the build.
      default = builtins.derivation {
        name = "cache-test-product";
        system = "x86_64-linux";
        builder = "${pkgs.bash}/bin/sh";
        args = [ "-c" ''
          ${pkgs.coreutils}/bin/mkdir -p $out/nix-support
          ${pkgs.coreutils}/bin/echo "cache test output" > $out/data
          ${pkgs.coreutils}/bin/printf 'file blob %s/data\n' "$out" > $out/nix-support/hydra-build-products
        '' ];
      };
    };
  };
}
