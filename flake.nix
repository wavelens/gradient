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
        ${defaultHostname} = self.nixosConfigurations.${defaultHostname}.config.microvm.declaredRunner;
        default = self.nixosConfigurations.${defaultHostname}.config.microvm.declaredRunner;
      };
      #test, run by 'nix run'
      nixosModules = {
        default = {
          imports = [
          ];
        };
      };
      nixosConfigurations."${defaultHostname}" = nixpkgs.lib.nixosSystem {
        system = "${system}";
        modules = [
          microvm.nixosModules.microvm
          self.nixosModules.default
          ./defaultNixConfig/example.nix
          ./defaultNixConfig/defaults.nix
          ./defaultNixConfig/postgresql.nix
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

          gettext
          libsodium
          openssl
          sqlite
          pythonEnv
        ];
      EXTRA_CCFLAGS = "-I/usr/include";
      RUST_BACKTRACE = 1;
    };
  };
}
