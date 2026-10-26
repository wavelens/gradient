/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, topology }: let
  DELAY_MS = 40;
  RATE = "100mbit";
  WORKER_CORES = 4;
  SERVER_CORES = 4;

  e2e = ../../gradient/e2e;

  workerToken = "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  workerIds = {
    worker1 = "00000000-0000-4000-8000-0000000000a1";
    worker2 = "00000000-0000-4000-8000-0000000000a2";
    worker3 = "00000000-0000-4000-8000-0000000000a3";
    worker4 = "00000000-0000-4000-8000-0000000000a4";
  };

  shapedLink = {
    systemd.services.shape-links = {
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      before = [ "gradient-worker.service" ];
      requiredBy = [ "gradient-worker.service" ];
      path = [ pkgs.iproute2 pkgs.kmod ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        modprobe -a ifb sch_netem sch_ingress cls_matchall act_mirred
        ip link show ifb0 >/dev/null 2>&1 || ip link add ifb0 type ifb
        ip link set dev ifb0 up
        tc qdisc replace dev eth1 root netem delay ${toString DELAY_MS}ms rate ${RATE}
        tc qdisc replace dev eth1 handle ffff: ingress
        tc filter replace dev eth1 parent ffff: matchall action mirred egress redirect dev ifb0
        tc qdisc replace dev ifb0 root netem delay ${toString DELAY_MS}ms rate ${RATE}
      '';
    };
  };

  workerModule = { lib, ... }: {
    imports = [ ../../../modules/gradient-worker.nix shapedLink ];

    virtualisation = {
      cores = WORKER_CORES;
      memorySize = 6144;
    };

    nix.settings = {
      trusted-users = [ "root" "@wheel" ];
      substituters = lib.mkForce [ ];
    };

    services.gradient.worker = {
      enable = true;
      eval.maxConcurrent = 1;
      capabilities = {
        eval = true;
        build = false;
      };
    };
  };

  topo = topology {
    inherit pkgs;
    inherit (pkgs) lib;
    token = workerToken;
    workers = pkgs.lib.mapAttrs (_: id: { inherit id; module = workerModule; }) workerIds;
  };

  benchTask = name: {
    project = "project";
    repository = "git://server/${name}";
    created_by = "admin";
    multi_node_evaluation = true;
    triggers = [{
      type = "time";
      config.cron = "0 0 3 1 1 *";
    }];
  };
in
assert import ../../harness/contract.nix { inherit (pkgs) lib; topology = topo; workers = workerIds; };
pkgs.testers.runNixOSTest ({ lib, ... }: {
  name = "gradient-evalbench-multinode";
  globalTimeout = 6 * 3600;

  defaults = {
    networking.firewall.enable = false;
    virtualisation = {
      diskSize = 8192;
      writableStore = true;
    };
    documentation.enable = false;
    nix.settings.max-jobs = 0;
  };

  nodes = {
    server = {
      imports = [
        (import ../server-node.nix {
          inherit self pkgs topo;
          token = workerToken;
          flakeFiles = {
            "large.nix" = ./flake_large.nix;
            "large.lock" = ./flake_large.lock;
            "small.nix" = "${e2e}/flake_repository.nix";
            "small.lock" = "${e2e}/flake_repository.lock";
          };
          tasks = {
            large = benchTask "large" // { wildcard = "hydraJobs.**"; };
            small = benchTask "small";
          };
        })
      ];

      virtualisation = {
        cores = SERVER_CORES;
        memorySize = 8192;
      };
    };
  } // topo.nodes;

  testScript = ''
    import importlib
    import json
    import os
    import pathlib
    import re
    import sys
    import time

    sys.path[:0] = ["${./.}", "${../evalbench}"]
    report = importlib.import_module("report")
    publish = importlib.import_module("summarize").publish

    GIT          = "${lib.getExe pkgs.git}"
    API          = "http://gradient.local/api/v1"
    NIXPKGS_HASH = "${self.inputs.nixpkgs.narHash}"
    DELAY_MS     = ${toString DELAY_MS}
    WORKER_CORES = ${toString WORKER_CORES}
    SERVER_CORES = ${toString SERVER_CORES}
    ${import ../../harness/prelude.nix { inherit lib; topology = topo; }}
    ${builtins.readFile ../bench_helpers.py}
    ${builtins.readFile ./test.py}
  '';
})
