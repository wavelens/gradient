/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

# `m1` and `m2` are waiting on `gate`, which is hanging until its release by the test.
# The cluster is seeded meanwhile.
# `gate` is building on zone-b's worker3, the cheapest seat for `m2`.
# Only `same_zone` is keeping `m2` in zone a.
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
