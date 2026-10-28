/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "These custom guest images support the Gradient exec VM test.";
  inputs = {
    nixpkgs.url = "http://server/nixpkgs.tar.gz";
    agent = {
      url = "http://server/gradient-guest.tar.gz";
      flake = false;
    };
    cli = {
      url = "http://server/gradient-cli.tar.gz";
      flake = false;
    };
  };
  outputs = { self, nixpkgs, agent, cli }: let
    pkgs = import nixpkgs { system = "x86_64-linux"; };
    programs = import ./apps.nix { inherit pkgs; };
    package = name: source: pkgs.runCommand name { } "cp -r ${source} $out";
    image = modules: nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./guest.nix
        ./trainer.nix
        {
          gradient.guest.package = package "gradient-guest" agent;
          nixpkgs.overlays = [ (_: _: { gradient-cli = package "gradient-cli" cli; }) ];
        }
      ] ++ modules;
    };
    app = name: gradient: {
      type = "app";
      program = "${programs.${name}}";
      passthru = { inherit gradient; };
    };
  in {
    nixosConfigurations = {
      trainer = image [ ];
      noagent = image [ { systemd.services.gradient-guest-agent.enable = false; } ];
    };
    apps.x86_64-linux = {
      trainer = app "trainer" { image = self.nixosConfigurations.trainer; };
      noagent = app "noagent" { image = self.nixosConfigurations.noagent; retries = 0; };
    };
  };
}
