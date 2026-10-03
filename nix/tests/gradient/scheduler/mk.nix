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

  # chain-4 is keeping the name chain-3. It is landing in chain-3's repo with c0-c2 unchanged.
  specFiles = lib.mapAttrs (_: import) {
    chain-3 = ./specs/chain.nix;
    chain-4 = ./specs/chain-4.nix;
    diamond-fail = ./specs/diamond-fail.nix;
    cross-worker = ./specs/cross-worker.nix;
    already-present = ./specs/already-present.nix;
    upstream-cached = ./specs/upstream-cached.nix;
    download = ./specs/download.nix;
    twins = ./specs/twins.nix;
    hang = ./specs/hang.nix;
    frozen = ./specs/frozen.nix;
    stress = ./specs/stress.nix;
    replay = ./specs/replay.nix;
  };
  specs = lib.attrValues specFiles;
  specNames = lib.unique (map (s: s.name) specs);

  flakes = lib.mapAttrs (_: storeSpec.toFlake) specFiles;
  resolved = lib.mapAttrs (_: storeSpec.resolve) specFiles;
  upstream = storeSpec.toUpstreamCache specs;
  downloads = storeSpec.toDownloads specs;

  workerToken = "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  workerIds = {
    worker1 = "dd9324a5-959e-40ee-82cc-df8dd880fa0b";
    worker2 = "3cd98f6a-c560-4617-8a61-1226859764af";
  };

  workerFeatures = {
    worker1 = [ "feature-a" ];
    worker2 = [ "feature-b" ];
  };

  workerNode = name: { ... }: {
    imports = [
      ../../../modules/gradient-worker.nix
      ./gradient-daemon.nix
    ];

    services.gradient-daemon-mock = {
      enable = true;
      package = daemon;
      config = storeSpec.toDaemonConfig specs name;
    };

    nix.settings.system-features = workerFeatures.${name};

    services.gradient.worker = {
      enable = true;
      system.features = workerFeatures.${name};
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

  serverNode = import ./server.nix {
    inherit lib pkgs storeSpec upstream downloads specNames workerToken;
    inherit (topo) upstreamPeers;
    upstreamUrls = topo.upstreamUrls or { };
  };
in
assert import ../../harness/contract.nix { inherit lib; topology = topo; workers = workerIds; };
pkgs.testers.runNixOSTest {
  name = "gradient-scheduler";
  globalTimeout = 3600;

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

  nodes = { server = serverNode; } // topo.nodes;

  testScript = ''
    import json
    FLAKES = json.loads(${builtins.toJSON (builtins.toJSON flakes)})
    RESOLVED = json.loads(${builtins.toJSON (builtins.toJSON resolved)})
    ${import ../../harness/prelude.nix { inherit lib; topology = topo; }}
    ${builtins.readFile ./helpers.py}
    ${builtins.readFile ./test.py}
  '';
}
