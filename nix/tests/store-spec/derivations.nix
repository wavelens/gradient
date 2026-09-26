/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

spec:
let
  inherit (builtins) mapAttrs attrNames attrValues hashString concatStringsSep concatMap filter head elemAt split;
  part = i: r: elemAt (split "\\." r) (i * 2);
  requestedOutputs = node: d:
    let
      referenced = map (part 1) (filter (r: part 0 r == d) (concatMap (o: o.references) (attrValues node.outputs)));
    in
    if referenced == [ ] then [ (head (attrNames spec.derivations.${d}.outputs)) ] else referenced;
  fodContent = id: "gradient-daemon fod ${spec.name}/${id}\n";
  drvs = mapAttrs (id: node:
    derivation ({
      inherit (node) name;
      inherit (spec) system;
      builder = "/bin/sh";
      args = [ "-c" "exit 1" ];
      gradientSpec = spec.name;
      gradientNode = id;
      deps = concatStringsSep " " (concatMap (d: map (o: "${drvs.${d}.${o}}") (requestedOutputs node d)) node.deps);
      inherit (node) requiredSystemFeatures preferLocalBuild allowSubstitutes;
    } // (if node.fixedOutput then {
      outputHashMode = "flat";
      outputHashAlgo = "sha256";
      outputHash = hashString "sha256" (fodContent id);
    } else {
      outputs = attrNames node.outputs;
    }))) spec.derivations;
in
{
  inherit drvs fodContent requestedOutputs;
}
