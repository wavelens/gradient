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
    # Hours of evaluation in flat memory, so only an abort ends it and the
    # worker's memory guard never reaps it.
    range = builtins.genList (i: i) 1000;
    loop = f: acc: builtins.foldl' (a: _: f a) acc range;
    spin = loop (loop (loop (acc: builtins.foldl' builtins.add acc range))) 0;
  in {
    packages.x86_64-linux.default = builtins.seq spin pkgs.hello;
  };
}
