/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

spec:
let
  inherit (builtins) mapAttrs attrNames hashString concatStringsSep;
  fodContent = id: "gradient-daemon fod ${spec.name}/${id}\n";
  drvs = mapAttrs (id: node:
    derivation ({
      inherit (node) name;
      inherit (spec) system;
      builder = "/bin/sh";
      args = [ "-c" "exit 1" ];
      gradientSpec = spec.name;
      gradientNode = id;
      deps = concatStringsSep " " (map (d: "${drvs.${d}}") node.deps);
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
  inherit drvs fodContent;
}
