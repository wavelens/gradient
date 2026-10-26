/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, topology }: let
  e2e = ../../gradient/e2e;

  testStore = import ../../../scripts/store.nix {
    inherit pkgs;
    skipDirectories = false;
  };

  workerToken = "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  workerIds.builder = "ab68d6ce-8331-44dc-9433-f22ccfd0a44b";

  builderModule = { lib, ... }: {
    imports = [ ../../../modules/gradient-worker.nix ../capture-module.nix ];

    virtualisation.additionalPaths = [ testStore ];

    nix.settings = {
      trusted-users = [ "root" "@wheel" ];
      max-jobs = lib.mkForce 4;
      substituters = lib.mkForce [ ];
    };

    services.gradient.worker = {
      enable = true;
      log.traceDir = "/var/lib/gradient-worker/trace";
      build.maxConcurrent = 4;
      capabilities = {
        eval = true;
        build = true;
      };
    };
  };

  topo = topology {
    inherit pkgs;
    inherit (pkgs) lib;
    token = workerToken;
    workers = pkgs.lib.mapAttrs (_: id: { inherit id; module = builderModule; }) workerIds;
  };
in
assert import ../../harness/contract.nix { inherit (pkgs) lib; topology = topo; workers = workerIds; };
pkgs.testers.runNixOSTest ({ lib, ... }: {
  name = "gradient-evalbench";
  globalTimeout = 3600;

  defaults = {
    networking.firewall.enable = false;
    virtualisation = {
      cores = 4;
      memorySize = 2048;
      diskSize = 8192;
      writableStore = true;
    };
    documentation.enable = false;
    nix.settings.max-jobs = 0;
  };

  nodes = {
    server = import ../server-node.nix {
      inherit self pkgs topo;
      token = workerToken;
      flakeFiles = {
        "flake.nix" = "${e2e}/flake_repository.nix";
        "flake.lock" = "${e2e}/flake_repository.lock";
      };
      tasks.task = {
        project = "project";
        repository = "git://server/test";
        created_by = "admin";
        triggers = [{
          type = "time";
          config.cron = "0 0 3 1 1 *";
        }];
      };
    };
  } // topo.nodes;

  testScript = ''
    import time

    GIT          = "${lib.getExe pkgs.git}"
    API          = "http://gradient.local/api/v1"
    INSPECTOR    = "${lib.getExe (pkgs.callPackage ../../../tools/evalbench-inspector { })}"
    NIXPKGS_HASH = "${self.inputs.nixpkgs.narHash}"
    ${import ../../harness/prelude.nix { inherit lib; topology = topo; }}
    ${builtins.readFile ../bench_helpers.py}
    ${builtins.readFile ./summarize.py}
    ${builtins.readFile ./capture.py}
    ${builtins.readFile ./test.py}
  '';
})
