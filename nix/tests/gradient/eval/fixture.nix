# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only

{
  description = "gradient eval-worker integration fixture";

  outputs = { self }:
    let
      row = builtins.genList (x: x) 10000;
      expensive = builtins.foldl' (a: _: builtins.foldl' (b: c: b + c) a row) 0 row;
    in
    {
      packages.x86_64-linux = {
        hello = derivation {
          name = "hello";
          system = "x86_64-linux";
          builder = "/bin/sh";
          args = [ (builtins.toString expensive) ];
        };

        cowsay = derivation {
          name = "cowsay";
          system = "x86_64-linux";
          builder = "/bin/sh";
        };

        # A trailing `*` is recovering this nested set one level deeper. `#` must not.
        nested.inner = derivation {
          name = "inner";
          system = "x86_64-linux";
          builder = "/bin/sh";
        };

        # Discovery must skip this evaluation error.
        # Resolve must report it per item without aborting the rest of the batch (#139).
        boom = throw "boom: this attribute must fail in isolation";

        imported = import (derivation {
          name = "ifd";
          system = "x86_64-linux";
          builder = "/bin/sh";
        });
      };
    };
}
