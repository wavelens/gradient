/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

# What mkSchedulerTest and mkE2eTest rely on; a topology from another repo is checked against it too.
{ lib, topology, workers }:
let
  clause = ok: what: lib.assertMsg ok "topology contract: ${what}";
in
clause (lib.all (name: topology.nodes ? ${name}) (lib.attrNames workers)) "every suite worker is a node"
&& clause (lib.sort lib.lessThan topology.workerNodes == lib.sort lib.lessThan (lib.attrNames workers)) "workerNodes are exactly the suite's workers"
&& clause (!(topology.nodes ? server)) "the suite owns the server node"
&& clause (lib.isAttrs topology.upstreamPeers && lib.all lib.isString (lib.attrValues topology.upstreamPeers)) "upstreamPeers maps names to worker ids"
&& clause (lib.isList topology.provides) "provides is a list of tags"
&& clause (lib.hasInfix "def wait_workers_ready(" topology.pythonPrelude) "the prelude defines wait_workers_ready"
&& clause (lib.hasInfix "def fleet_units(" topology.pythonPrelude) "the prelude defines fleet_units"
