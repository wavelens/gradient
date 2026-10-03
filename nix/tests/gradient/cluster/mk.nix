/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, topology }:
let
  inherit (pkgs) lib;

  daemon = self.packages.${pkgs.stdenv.hostPlatform.system}.gradient.daemon;
  storeSpec = import ../../store-spec { inherit pkgs lib daemon; };

  specFiles = lib.mapAttrs (_: import) {
    zone-pair = ./specs/zone-pair.nix;
    kill-pair = ./specs/kill-pair.nix;
  };
  specs = lib.attrValues specFiles;

  flakes = lib.mapAttrs (_: storeSpec.toFlake) specFiles;
  resolved = lib.mapAttrs (_: storeSpec.resolve) specFiles;

  workerToken = "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  workerIds = {
    worker1 = "dd9324a5-959e-40ee-82cc-df8dd880fa0b";
    worker2 = "3cd98f6a-c560-4617-8a61-1226859764af";
    worker3 = "7f3c9e21-4b8a-4d5e-9c1f-2a6b8d0e4f13";
  };
  zones = { worker1 = "a"; worker2 = "a"; worker3 = "b"; };
  features = { worker1 = [ "big-parallel" ]; worker2 = [ "big-parallel" ]; worker3 = [ "big-parallel" "zone-b" ]; };

  workerNode = name: { ... }: {
    imports = [
      ../../../modules/gradient-worker.nix
      ../scheduler/gradient-daemon.nix
    ];

    services.gradient-daemon-mock = {
      enable = true;
      package = daemon;
      config = storeSpec.toDaemonConfig specs name;
    };

    nix.settings.system-features = features.${name};

    services.gradient.worker = {
      enable = true;
      zone = zones.${name};
      system.features = features.${name};
      capabilities = {
        eval = true;
        build = true;
      };
    };
  };

  topo = topology {
    inherit pkgs lib;
    token = workerToken;
    workers = lib.mapAttrs (name: id: { inherit id; module = workerNode name; }) workerIds;
  };
in
assert import ../../harness/contract.nix { inherit lib; topology = topo; workers = workerIds; };
pkgs.testers.runNixOSTest {
  name = "gradient-cluster";
  globalTimeout = 2400;

  defaults = {
    networking.firewall.enable = false;
    virtualisation = {
      cores = 2;
      memorySize = 2048;
      diskSize = 4096;
      writableStore = true;
    };
    documentation.enable = false;
    nix.settings.substituters = lib.mkForce [ ];
  };

  nodes = {
    server = import ../scheduler/server.nix {
      inherit lib pkgs storeSpec specs workerToken;
      inherit (topo) upstreamPeers;
      upstreamUrls = topo.upstreamUrls or { };
    };
  } // topo.nodes;

  testScript = ''
    import json
    FLAKES = json.loads(${builtins.toJSON (builtins.toJSON flakes)})
    RESOLVED = json.loads(${builtins.toJSON (builtins.toJSON resolved)})
    WORKER_IDS = json.loads(${builtins.toJSON (builtins.toJSON workerIds)})
    ${import ../../harness/prelude.nix { inherit lib; topology = topo; }}
    ${builtins.readFile ../scheduler/helpers.py}
    ${builtins.readFile ./test.py}
  '';
}
