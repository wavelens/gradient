/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "Exec apps support the Gradient exec VM test.";
  inputs.nixpkgs.url = "http://server/nixpkgs.tar.gz";
  outputs = { nixpkgs, ... }: let
    pkgs = import nixpkgs { system = "x86_64-linux"; };
    programs = import ./apps.nix { inherit pkgs; };
    app = name: gradient: {
      type = "app";
      program = "${programs.${name}}";
      passthru = { inherit gradient; };
    };
  in {
    apps.x86_64-linux = {
      fetch = app "fetch" { labels = [ "stop" ]; network = true; };
      silent = app "silent" { labels = [ "stop" ]; maxSilent = 5; };
      abort = app "abort" { labels = [ "stop" ]; };
      long-stop = app "long-stop" { labels = [ "stop" ]; };
      fail = app "fail" { labels = [ "keep" ]; retries = 1; };
      long-keep = app "long-keep" { labels = [ "keep" ]; };
      lost = app "lost" { labels = [ "keep" ]; };
    };
  };
}
