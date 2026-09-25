/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

let
  p = import ../../../store-spec/presets.nix;
  chain = p.chain 2;
in
chain // {
  name = "frozen";
  derivations = chain.derivations // {
    c1 = chain.derivations.c1 // { build.outcome = "hang"; };
  };
}
