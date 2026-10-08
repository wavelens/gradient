/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  outputs = { self, ... }: {
    packages."@system@" = let
      mk = name: deps: builtins.derivation {
        inherit name;
        system = "@system@";
        builder = "/bin/sh";
        args = [ "-c" "echo ${toString deps} ${name} > $out" ];
      };
      base = mk "base" [ ];
      left = mk "left" [ base ];
      right = mk "right" [ base ];
      ifd = builtins.derivation {
        name = "ifd";
        system = "@system@";
        builder = "/bin/sh";
        args = [ "-c" "echo '\"imported\"' > $out" ];
      };
    in {
      inherit base left right;
      top = mk "top" [ left right ];
      imported = mk (import ifd) [ base ];
    };
  };
}
