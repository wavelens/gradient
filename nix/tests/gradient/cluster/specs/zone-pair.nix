/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

# `m1` and `m2` wait on `gate`, which hangs until the test releases it: the cluster is seeded meanwhile.
{
  name = "zone-pair";
  derivations = {
    gate.build.outcome = "hang";
    m1 = { deps = [ "gate" ]; outputs.out.references = [ "gate.out" ]; };
    m2 = { deps = [ "gate" ]; outputs.out.references = [ "gate.out" ]; };
  };
  entryPoints = [ "m1" "m2" ];
}
