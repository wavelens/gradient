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
  identity = id: let twin = spec.derivations.${id}.sameAs or null; in if twin == null then id else twin;
  fodContent = id: "gradient-daemon fod ${spec.name}/${identity id}\n";
  download = id: import <nix/fetchurl.nix> {
    inherit (spec.derivations.${identity id}) name;
    url = "http://server/downloads/${spec.name}/${identity id}";
    hash = builtins.convertHash { hash = hashString "sha256" (fodContent id); hashAlgo = "sha256"; toHashFormat = "sri"; };
  };
  drvs = mapAttrs (id: node:
    if node.download then download id else
    derivation ({
      inherit (spec.derivations.${identity id}) name;
      inherit (spec) system;
      builder = "/bin/sh";
      args = [ "-c" "exit 1" ];
      gradientSpec = spec.name;
      gradientNode = if node.fixedOutput or false then id else identity id;
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
