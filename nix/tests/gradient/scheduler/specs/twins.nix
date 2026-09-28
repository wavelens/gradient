/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

# `lib1` and `lib2` are twins: one derivation over two fixed-output sources that
# differ only in their `.drv`, so both produce the same output paths. Only `lib1`
# is exported upstream, which serves `lib2`'s shared paths as well.
{
  name = "twins";
  derivations = {
    src1 = { fixedOutput = true; name = "src"; };
    src2 = { fixedOutput = true; sameAs = "src1"; };
    lib1 = {
      name = "lib";
      deps = [ "src1" ];
      outputs = { out = { }; dev.references = [ "lib1.out" ]; };
      present.cache = true;
    };
    lib2 = {
      sameAs = "lib1";
      deps = [ "src2" ];
      outputs = { out = { }; dev.references = [ "lib2.out" ]; };
    };
    app1 = { deps = [ "lib1" ]; outputs.out.references = [ "lib1.dev" ]; };
    app2 = { deps = [ "lib2" ]; outputs.out.references = [ "lib2.dev" ]; };
  };
  entryPoints = [ "app1" "app2" ];
}
