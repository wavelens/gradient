# SPDX-License-Identifier: MIT-0
# SPDX-FileCopyrightText: 2023 Alyssa Ross <hi@alyssa.is>
# SPDX-FileCopyrightText: 2024 embr <git@liclac.eu>

{ pkgs ? import ../nix/nixpkgs.default.nix
, lib ? pkgs.lib
, nix-supervisor ? pkgs.callPackage ../nix-supervisor { buildType = "debug"; }
, cargo ? pkgs.cargo
, hello ? pkgs.hello
}:

let
  isPackage = lib.types.package.check;
  nixPackages = lib.filterAttrs (_: isPackage) (pkgs.callPackage ../nix/nix-packages.nix {});

  # Build the nix integration tests into a standalone binary.
  test-nix =
    pkgs.rustPlatform.buildRustPackage {
      name = "test-nix";
      buildType = "debug";
      inherit (nix-supervisor) src buildInputs nativeBuildInputs;
      cargoDepsName = "nix-supervisor";
      cargoLock.lockFile = ../Cargo.lock;

      cargoBuildFlags = [ "-p=nix-daemon" "--test=nix" ];
      dontCargoCheck = true;
      installPhase = ''
        mkdir -p $out/bin
        bin=$(find target/*/release/deps/ -type f -executable | grep -E 'nix-[0-9a-z]+$')
        echo "found test runner binary: $bin"
        cp $bin $out/bin/nix-integration
      '';

      # Binary cache used for the substitution tests.
      passthru.binary-cache =
        (pkgs.mkBinaryCache {
          name = "test-binary-cache";
          rootPaths = with pkgs; [ hello ];
        }).overrideAttrs ({ buildCommand, ... }: {
          buildCommand = buildCommand + ''
            echo 'WantMassQuery: 1' >> $out/nix-cache-info
          '';
        });
    };

  # Generate a nix integration test for the given nix package.
  mkNixTest = attr: nix: pkgs.testers.nixosTest ({ lib, ... }: {
    name = "nix-daemon-test-${attr}";

    nodes.machine = { modulesPath, ... }: {
      imports = [
        "${modulesPath}/installer/cd-dvd/channel.nix"
      ];

      environment.systemPackages = [ hello ];
      system.extraDependencies = [ hello.drvPath ];

      users.users.testuser = {
        isNormalUser = true;
      };

      nix.package = nix;
      nix.settings = {
        substituters = lib.mkForce [ "file://${test-nix.binary-cache}" ];
        trusted-substituters = [ "testuser" ];
        trusted-users = [ "testuser" ];
      };
    };
    testScript = { nodes, ... }: ''
      print(machine.succeed('nix --version'))
      print(machine.succeed('cat /etc/nix/nix.conf'))

      # Run the nix integration tests!
      print(machine.succeed('sudo -u testuser env -C $(sudo -u testuser mktemp -d) ${test-nix}/bin/nix-integration'))
    '';
  });
in
{
  inherit test-nix;
} // (lib.mapAttrs mkNixTest nixPackages)
