/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
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

    nixVersion = pkgs.nixVersions.nix_2_34;

    rustEnv = with pkgs.rustPackages; [
      clippy
    ];
  in
  {
    checks = import ./nix/tests { inherit self inputs system pkgs; };
    apps = import ./nix/vms { inherit inputs system pkgs; };
    packages = rec {
      store = pkgs.callPackage ./nix/scripts/store.nix { };
      gradient = pkgs.callPackage ./nix/packages/gradient.nix { };
      gradient-frontend = pkgs.callPackage ./nix/packages/gradient-frontend.nix { };
      gradient-cli = pkgs.callPackage ./nix/packages/gradient-cli.nix { };
      default = gradient;
    };

    devShells.default = with pkgs; mkShell {
      buildInputs = [
        stdenv.cc.cc.lib
        pam
        nixVersion
      ];

      packages = [
        cargo
        cargo-llvm-cov
        cargo-nextest
        pkg-config
        rustc
        rustfmt
        sea-orm-cli
        rustEnv

        llvmPackages.lld
        lldb

        http-server
        nodejs
        pnpm

        openssl
        sqlite
        postgresql_18
        pgadmin4-desktopmode
        zstd
      ];

      nativeBuildInputs = [
        nixVersion.dev
        pkg-config
        glibc.dev
      ];

      LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
      BINDGEN_EXTRA_CLANG_ARGS = "--sysroot=${glibc.dev}";

      LLVM_COV = "${llvmPackages.llvm}/bin/llvm-cov";
      LLVM_PROFDATA = "${llvmPackages.llvm}/bin/llvm-profdata";

      EXTRA_CCFLAGS = "-I/usr/include";
      RUST_BACKTRACE = 1;

      GRADIENT_DEBUG = "true";
      GRADIENT_SERVE_URL = "http://localhost:3000";
      GRADIENT_DATABASE_URL = "postgres://postgres:postgres@localhost:54321/gradient";
      GRADIENT_MAX_CONCURRENT_EVALUATIONS = 1;
      GRADIENT_MAX_CONCURRENT_BUILDS = 8;
      GRADIENT_STORE_PATH = "./testing/store";
      GRADIENT_CRYPT_SECRET_FILE = pkgs.writeText "crypt_secret_file" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK";
      GRADIENT_JWT_SECRET_FILE = pkgs.writeText "jwt_secret_file" "8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9d";
      GRADIENT_REPORT_ERRORS = "false";
    };
  }) // {
    overlays = {
      gradient = final: prev: { inherit (self.packages.${final.stdenv.hostPlatform.system}) gradient; };
      gradient-frontend = final: prev: { inherit (self.packages.${final.stdenv.hostPlatform.system}) gradient-frontend; };
      gradient-cli = final: prev: { inherit (self.packages.${final.stdenv.hostPlatform.system}) gradient-cli; };
      default = final: prev: { inherit (self.packages.${final.stdenv.hostPlatform.system}) gradient gradient-frontend gradient-cli; };
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
