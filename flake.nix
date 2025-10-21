/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{
  description = "nix-based continuous integration system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils, ... }@inputs: flake-utils.lib.eachDefaultSystem (system: let
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
      django-compression-middleware
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
      sentry-sdk
      uritemplate
      urllib3
      whitenoise
      xstatic-bootstrap
      xstatic-jquery
      xstatic-jquery-ui
    ]);
  in
  {
    checks = import ./nix/tests { inherit self inputs system pkgs; };
    apps = import ./nix/vms { inherit inputs system pkgs; };
    packages = rec {
      gradient-server = pkgs.callPackage ./nix/packages/gradient-server.nix { };
      gradient-frontend = pkgs.callPackage ./nix/packages/gradient-frontend.nix { };
      gradient-cli = pkgs.callPackage ./nix/packages/gradient-cli.nix { };
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

        black
        boost
        gettext
        libsodium
        openssl
        sqlite
        pythonEnv
        postgresql_18
        pgadmin4-desktopmode
        nixVersions.latest
        zstd
      ];

      nativeBuildInputs = [
        pkg-config
      ];

      EXTRA_CCFLAGS = "-I/usr/include";
      RUST_BACKTRACE = 1;

      GRADIENT_DEBUG = "true";
      GRADIENT_SERVE_URL = "http://localhost:3000";
      GRADIENT_DATABASE_URL = "postgres://postgres:postgres@localhost:54321/gradient";
      GRADIENT_MAX_CONCURRENT_EVALUATIONS = 1;
      GRADIENT_MAX_CONCURRENT_BUILDS = 10;
      GRADIENT_STORE_PATH = "./testing/store";
      GRADIENT_CRYPT_SECRET_FILE = pkgs.writeText "crypt_secret_file" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK";
      GRADIENT_JWT_SECRET_FILE = pkgs.writeText "jwt_secret_file" "8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9d";
      GRADIENT_SERVE_CACHE = "true";
      GRADIENT_REPORT_ERRORS = "false";
    };
  }) // {
    overlays = {
      gradient-server = final: prev: { inherit (self.packages.${final.system}) gradient-server; };
      gradient-frontend = final: prev: { inherit (self.packages.${final.system}) gradient-frontend; };
      gradient-cli = final: prev: { inherit (self.packages.${final.system}) gradient-cli; };
      default = final: prev: { inherit (self.packages.${final.system}) gradient-server gradient-frontend gradient-cli; };
    };

    nixosModules = rec {
      deploy = ./nix/modules/gradient-deploy.nix;
      gradient = { config, lib, ... }: {
        imports = [ ./nix/modules/gradient.nix ];
        nixpkgs.overlays = lib.mkIf config.services.gradient.enable [
          self.overlays.default
        ];
      };

      default = gradient;
    };
  };
}
