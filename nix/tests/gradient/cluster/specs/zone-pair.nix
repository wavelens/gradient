/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

# `m1` and `m2` wait on `gate`, which hangs until the test releases it: the cluster is seeded meanwhile.
# `gate` builds on zone-b's worker3, the cheapest seat for `m2`, so only `same_zone` keeps `m2` in zone a.
{
  name = "zone-pair";
  derivations = {
    gate = {
      build.outcome = "hang";
      requiredSystemFeatures = [ "zone-b" ];
    };
    m1 = { deps = [ "gate" ]; outputs.out.references = [ "gate.out" ]; };
    m2 = { deps = [ "gate" ]; outputs.out.references = [ "gate.out" ]; };
  };
  entryPoints = [ "m1" "m2" ];
}
