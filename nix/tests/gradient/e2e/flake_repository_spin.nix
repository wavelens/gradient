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
    # Hours of pure evaluation, so only an abort can end it.
    spin = builtins.foldl'
      (acc: _: builtins.foldl' builtins.add acc (builtins.genList (i: i) 10000))
      0
      (builtins.genList (i: i) 10000000);
  in {
    packages.x86_64-linux.default = builtins.seq spin pkgs.hello;
  };
}
