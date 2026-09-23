/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

let
  node = deps: { inherit deps; };
  range = n: builtins.genList (i: i) n;
  mod = a: b: a - b * (a / b);
  cell = d: w: "n${toString d}_${toString w}";
in
{
  chain = n: {
    name = "chain-${toString n}";
    derivations = builtins.listToAttrs (map
      (i: {
        name = "c${toString i}";
        value = node (if i == 0 then [ ] else [ "c${toString (i - 1)}" ]) // {
          outputs.out.references = if i == 0 then [ ] else [ "c${toString (i - 1)}.out" ];
        };
      })
      (range n));
    entryPoints = [ "c${toString (n - 1)}" ];
  };

  diamond = {
    name = "diamond";
    derivations = {
      base = node [ ];
      left = node [ "base" ] // { outputs.out.references = [ "base.out" ]; };
      right = node [ "base" ] // { outputs.out.references = [ "base.out" ]; };
      top = node [ "left" "right" ];
    };
    entryPoints = [ "top" ];
  };

  fanOut = n: {
    name = "fan-out-${toString n}";
    derivations = { root = node [ ]; } // builtins.listToAttrs (map
      (i: { name = "leaf${toString i}"; value = node [ "root" ]; })
      (range n));
  };

  # Each cell depends on its own column and the next one (wrapping), so levels form a mesh.
  wide = depth: width: {
    name = "wide-${toString depth}x${toString width}";
    derivations = builtins.listToAttrs (builtins.concatMap
      (d: map
        (w: {
          name = cell d w;
          value = node (if d == 0 then [ ] else
            if width == 1 then [ (cell (d - 1) w) ] else
            [ (cell (d - 1) w) (cell (d - 1) (mod (w + 1) width)) ]);
        })
        (range width))
      (range depth));
    entryPoints = map (cell (depth - 1)) (range width);
  };
}
