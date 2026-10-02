/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

# Both members are hanging once started. One member can be lost mid-attempt.
# The test is overriding the retry to succeed.
{
  name = "kill-pair";
  derivations = {
    gate.build.outcome = "hang";
    k1 = { deps = [ "gate" ]; outputs.out.references = [ "gate.out" ]; build.outcome = "hang"; };
    k2 = { deps = [ "gate" ]; outputs.out.references = [ "gate.out" ]; build.outcome = "hang"; };
  };
  entryPoints = [ "k1" "k2" ];
}
