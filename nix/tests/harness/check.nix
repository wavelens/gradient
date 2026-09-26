/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, lib }:
let
  workers = {
    a = { id = "id-a"; module = { }; };
    b = { id = "id-b"; module = { }; };
  };
  direct = import ./topologies/direct.nix { inherit pkgs lib workers; token = "t"; };
  prelude = import ./prelude.nix { inherit lib; topology = direct; };
in
assert import ./contract.nix { inherit lib workers; topology = direct; };
assert direct.upstreamPeers == { a = "id-a"; b = "id-b"; };
assert direct.provides == [ "distinct-upstream-workers" ];
assert direct.nodes.a.services.gradient.worker.workerId == "id-a";
assert direct.nodes.a.services.gradient.worker.serverUrl == "ws://server/proto";
assert direct.nodes.b.environment.etc."gradient/secrets/worker_peers".text == "*:t";
assert lib.hasInfix "WORKER_NODES = [a, b]" prelude;
assert lib.hasInfix ''PROVIDES = set(["distinct-upstream-workers"])'' prelude;
assert lib.hasInfix "def requires(" prelude;
pkgs.runCommand "test-topologies-check" { } "touch $out"
