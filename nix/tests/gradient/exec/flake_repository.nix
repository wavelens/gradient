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
    # Jobs ask for the size of the 1-vCPU warm guests, since 2-vCPU guests booting together outrun the boot timeout on the 2-core workers.
    warmGuestSize = { cpus = 1; memoryMb = 500; };
    app = name: gradient: {
      type = "app";
      program = "${programs.${name}}";
      passthru.gradient = warmGuestSize // gradient;
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
      sized = app "sized" { labels = [ "stop" ]; cpus = 2; memoryMb = 768; diskMb = 256; };
      toolarge = app "toolarge" { labels = [ "stop" ]; cpus = 64; };
      pair = app "pair" {
        nodes = 2;
        labels = [ "pair" ];
        network = true;
        args = [ "@gradient.cluster.master_addr@:@gradient.cluster.master_port@" ];
      };
    };
    packages.x86_64-linux.hello = programs.hello;
  };
}
