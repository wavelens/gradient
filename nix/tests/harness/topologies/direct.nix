/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

# Every worker dials the server itself, so the server sees each one as its own worker.
{ lib, workers, token, ... }:
{
  nodes = lib.mapAttrs (_: worker: {
    imports = [ worker.module ];

    environment.etc."gradient/secrets/worker_peers" = {
      mode = "0600";
      user = "gradient-worker";
      group = "gradient-worker";
      text = "*:${token}";
    };

    services.gradient.worker = {
      serverUrl = "ws://server/proto";
      workerId = worker.id;
      peersFile = "/etc/gradient/secrets/worker_peers";
    };
  }) workers;

  upstreamPeers = lib.mapAttrs (_: worker: worker.id) workers;
  workerNodes = lib.attrNames workers;
  provides = [ "distinct-upstream-workers" ];

  pythonPrelude = ''
    def fleet_units():
        return [(node, "gradient-worker") for node in WORKER_NODES]


    def wait_workers_ready():
        for node in WORKER_NODES:
            node.wait_for_unit("gradient-worker.service")
            node.wait_until_succeeds(
                "journalctl -u gradient-worker --no-pager | grep -q 'handshake successful'", timeout=180
            )
  '';
}
