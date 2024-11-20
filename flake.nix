{
  description = "Nix Build Server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    utils.url = "github:numtide/flake-utils";
    microvm = {
      url = "github:astro/microvm.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs = { self, nixpkgs, microvm, ... }:
    let
      system = "x86_64-linux";
      defaultHostname = "gradient-dev";
      pkgs = import nixpkgs { system = "${system}"; config = { allowUnfree = true; }; };
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
      packages."${system}" = {
        "vm-${defaultHostname}" = self.nixosConfigurations.${defaultHostname}.config.microvm.declaredRunner;
        db = pkgs.callPackage ./nix/scripts/postgres.nix { };
      };

      nixosConfigurations."${defaultHostname}" = nixpkgs.lib.nixosSystem {
        system = "${system}";
        modules = [
          microvm.nixosModules.microvm
          ./nix/vm/base.nix
          ./nix/vm/defaults.nix
          ./nix/vm/postgresql.nix
          ./nix/vm/mDNS.nix
          ./nix/vm/monitoring/grafana/nginx.nix
          ./nix/vm/monitoring/grafana/default.nix
          ./nix/vm/monitoring/prometheus/default.nix
        ];
      };

      devShells."${system}".default = with pkgs; mkShell {
        buildInputs = [
          stdenv.cc.cc.lib
          pam
        ];

        packages = [
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
        ];

        nativeBuildInputs = [
          pkg-config
        ];

        EXTRA_CCFLAGS = "-I/usr/include";
        RUST_BACKTRACE = 1;

        GRADIENT_DATABASE_URL = "postgres://postgres:postgres@localhost:54321/gradient";
        GRADIENT_MAX_CONCURRENT_EVALUATIONS = 1;
        GRADIENT_MAX_CONCURRENT_BUILDS = 10;
        GRADIENT_STORE_PATH = "./testing/store";
    };
  };
}
