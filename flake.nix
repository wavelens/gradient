/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{
  description = "nix-based continuous integration system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    microvm = {
      url = "github:astro/microvm.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs = { self, nixpkgs, microvm, flake-utils, ... }@inputs: flake-utils.lib.eachDefaultSystem (system: let
    pkgs = import nixpkgs {
      inherit system;
      overlays = map (v: self.overlays.${v}) (builtins.attrNames self.overlays);
      config = { allowUnfree = true; };
    };

    rustEnv = with pkgs.rustPackages; [
      clippy
    ];

    py = pkgs.python3.override {
      packageOverrides = python-final: python-prev: {
        django = python-final.django_4;
      };
    };

    pythonEnv = py.withPackages (ps: with ps; [
      bleach
      celery
      channels
      channels-redis
      django
      django-debug-toolbar
      django-parler
      django-redis
      django-rosetta
      django-scheduler
      gunicorn
      mysqlclient
      redis
      requests
      selenium
      uritemplate
      urllib3
      xstatic-bootstrap
      xstatic-jquery
      xstatic-jquery-ui
    ]);
  in
  {
    checks = import ./nix/tests { inherit inputs system pkgs; };
    packages = rec {
      gradient-server = pkgs.callPackage ./nix/packages/gradient-server.nix { };
      gradient-frontend = pkgs.callPackage ./nix/packages/gradient-frontend.nix { };
      gradient-cli = pkgs.callPackage ./nix/packages/gradient-cli.nix { };
      "vm-gradient-dev" = self.nixosConfigurations."gradient-dev".config.microvm.declaredRunner;
      db = pkgs.callPackage ./nix/scripts/postgres.nix { };
      gradient = gradient-cli;
      default = gradient-server;
    };

    devShells.default = with pkgs; mkShell {
      buildInputs = [
        stdenv.cc.cc.lib
        pam
      ];

      packages = [
        cargo
        pkg-config
        rustc
        rustfmt
        sea-orm-cli
        rustEnv

        boost
        gettext
        libsodium
        openssl
        sqlite
        pythonEnv
        postgresql_17
        pgadmin4-desktopmode
        nixVersions.latest
        protobuf
      ];

      nativeBuildInputs = [
        pkg-config
      ];

      EXTRA_CCFLAGS = "-I/usr/include";
      PROTOC = "${protobuf}/bin/protoc";
      PROTO_ROOT = "/home/dennis/repos/tvix-depot/";
      TVIX_BUILD_SANDBOX_SHELL = "/bin/sh";
      RUST_BACKTRACE = 1;

      GRADIENT_DEBUG = "true";
      GRADIENT_DATABASE_URL = "postgres://postgres:postgres@localhost:54321/gradient";
      GRADIENT_MAX_CONCURRENT_EVALUATIONS = 1;
      GRADIENT_MAX_CONCURRENT_BUILDS = 10;
      GRADIENT_STORE_PATH = "./testing/store";
      GRADIENT_JWT_SECRET = "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a";
      GRADIENT_CRYPT_SECRET = "aW52YWxpZAo=";
    };
  }) // {
    overlays = rec {
      gradient-server = final: prev: { inherit (self.packages.${final.system}) gradient-server; };
      gradient-frontend = final: prev: { inherit (self.packages.${final.system}) gradient-frontend; };
      gradient-cli = final: prev: { inherit (self.packages.${final.system}) gradient-cli; };
      default = gradient-server;
    };

    nixosModules = rec {
      gradient = { config, lib, ... }: {
        imports = [ ./nix/modules/gradient.nix ];
        nixpkgs.overlays = lib.mkIf config.services.gradient.enable [
          self.overlays.default
        ];
      };

      default = gradient;
    };

    nixosConfigurations."gradient-dev" = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        microvm.nixosModules.microvm
        ./nix/vm/base.nix
        ./nix/vm/defaults.nix
        ./nix/vm/postgresql.nix
        ./nix/vm/mDNS.nix
        ./nix/vm/nginx/default.nix
        ./nix/vm/nginx/grafana.nix
        ./nix/vm/monitoring/source/prometheus/default.nix
        ./nix/vm/monitoring/source/loki/default.nix
        ./nix/vm/monitoring/destination/grafana/default.nix
      ];
    };
  };
}
