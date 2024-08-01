{
  description = "Nix Build Server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    utils.url = "github:numtide/flake-utils";
  };

  outputs = { nixpkgs, ... }:
    let
      pkgs = import nixpkgs { system = "x86_64-linux"; config = { allowUnfree = true; }; };
      rustEnv = with pkgs.rustPackages; [ clippy ];
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
        django-shortuuidfield
        gunicorn
        model-bakery
        mysqlclient
        redis
        requests
        uritemplate
        urllib3
        xstatic-bootstrap
        xstatic-jquery
        xstatic-jquery-ui
      ]);
    in
    {
      devShells.x86_64-linux.default = with pkgs; mkShell {
        buildInputs = [
          stdenv.cc.cc.lib
          pam
        ];

        packages = [
          rustup
          pkg-config
          openssl
          libsodium
          rustfmt
          rustEnv

          gettext
          pythonEnv
        ];

        EXTRA_CCFLAGS = "-I/usr/include";
        RUST_BACKTRACE = 1;
      };
    };
}
