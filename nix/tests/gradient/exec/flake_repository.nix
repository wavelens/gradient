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
      interactive = app "interactive" { labels = [ "keep" ]; ssh = true; ports = [ 8080 ]; };
      terminal = app "terminal" { labels = [ "keep" ]; ssh = true; };
      secrets = app "secrets" {
        labels = [ "stop" ];
        secrets = [ "api-token" "wandb-token" "lab-key" ];
        sops = [ ./secrets/train.yaml ];
        args = [ "@gradient.secret.wandb-token@" ];
      };
      clash = app "clash" { labels = [ "stop" ]; secrets = [ "clash-key" ]; };
      outputs = app "outputs" { labels = [ "stop" ]; network = true; };
    };
    packages.x86_64-linux.hello = programs.hello;
  };
}
