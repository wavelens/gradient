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
  name = "already-present";
  derivations = builtins.mapAttrs (_: n: n // { present.workers = [ "worker1" "worker2" ]; }) chain.derivations;
}
