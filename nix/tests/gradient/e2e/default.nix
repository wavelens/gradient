/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, ... }:
{
  value = import ./mk.nix {
    inherit self pkgs;
    topology = import ../../harness/topologies/direct.nix;
  };
}
