/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

drv:
let
  isDerivation = builtins.isAttrs drv && (drv.type or null) == "derivation";
  name = drv.meta.mainProgram or drv.pname or (builtins.parseDrvName drv.name).name;
  bin = if drv.outputSpecified or false then drv else drv.bin or drv.out or drv;
in
if isDerivation then "${bin}/bin/${name}"
else throw "The required inputs for gradient run are a package, an exec app, or a devShell, not ${builtins.typeOf drv}."
