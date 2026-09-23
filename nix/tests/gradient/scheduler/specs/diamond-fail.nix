/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

let
  p = import ../../../store-spec/presets.nix;
in
p.diamond // {
  name = "diamond-fail";
  derivations = p.diamond.derivations // {
    left = p.diamond.derivations.left // { build.outcome = "fail"; };
  };
}
