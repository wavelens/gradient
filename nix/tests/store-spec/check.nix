/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, lib }:
let
  s = import ./. { inherit pkgs lib; daemon = null; };
  throws = e: !(builtins.tryEval (builtins.deepSeq e e)).success;
  chain = s.resolve (s.presets.chain 3);
  reimported = import (builtins.toFile "store-spec.nix" (lib.generators.toPretty { } (s.normalize (s.presets.chain 3))));
  flakeDrvs = (import ./derivations.nix reimported).drvs;
  multi = (import ./derivations.nix (s.normalize { name = "multi"; derivations = { a.outputs = { dev = { }; lib = { }; }; b = { deps = [ "a" ]; outputs.out.references = [ "a.lib" ]; }; }; })).drvs;
in
assert throws (s.normalize { name = "cyc"; derivations = { a.deps = [ "b" ]; b.deps = [ "a" ]; }; });
assert throws (s.normalize { name = "dangling"; derivations = { a.deps = [ "zz" ]; }; });
assert throws (s.normalize { name = "outside"; derivations = { a = { }; b.outputs.out.references = [ "a.out" ]; }; });
assert throws (s.normalize { name = "build-only"; derivations = { a = { }; b.deps = [ "a" ]; c = { deps = [ "b" ]; outputs.out.references = [ "a.out" ]; }; }; });
assert (s.normalize { name = "runtime"; derivations = { a = { }; b = { deps = [ "a" ]; outputs.out.references = [ "a.out" ]; }; c = { deps = [ "b" ]; outputs.out.references = [ "a.out" ]; }; }; }) ? derivations;
assert builtins.attrValues (builtins.getContext multi.b.deps) == [ { outputs = [ "lib" ]; } ];
assert throws (s.normalize { name = "fod-refs"; derivations = { a = { }; b = { deps = [ "a" ]; fixedOutput = true; outputs.out.references = [ "a.out" ]; }; }; });
assert throws (s.normalize { derivations = { }; });
assert chain.derivations.c2.outputs.out.references == [ chain.derivations.c1.outputs.out.path ];
assert flakeDrvs.c2.drvPath == chain.derivations.c2.drvPath;
assert builtins.all (id: (s.resolve (s.presets.wide 3 4)).derivations.${id}.drvPath != "") (builtins.attrNames (s.presets.wide 3 4).derivations);
pkgs.runCommand "store-spec-check" { __structuredAttrs = true; } "touch $out"
