/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

let p = import ../../../store-spec/presets.nix; in (p.chain 4) // { name = "chain-3"; }
