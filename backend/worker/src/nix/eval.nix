flake_ref: wildcard_ref: let
  flake = (builtins.getFlake flake_ref).outputs;

  range = first: last: if first > last then
    [ ]
  else
    builtins.genList (n: first + n) (last - first + 1);

  # Cheap attribute peek — does not fall through to module evaluation when the
  # value lacks `.type`. Wrapped in tryEval at call sites because forcing
  # `value.type` itself can still throw on broken NixOS modules.
  isDerivation = value: value.type or null == "derivation";

  # tryEval-wrapped helpers used during traversal so that one failing attribute
  # (e.g. a NixOS module that throws) does not abort the entire listing.
  safeIsDerivation = v: let
    p = builtins.tryEval (isDerivation v);
  in p.success && p.value;

  safeAttrNames = v: let
    p = builtins.tryEval (
      if builtins.isAttrs v then builtins.attrNames v else [ ]
    );
  in if p.success then p.value else [ ];

  safeGetAttr = name: v: builtins.tryEval (builtins.getAttr name v);

  # Check whether a path (list of strings) matches an exclude pattern (list of strings).
  matchesExclude = path: pattern: (
    builtins.length path == builtins.length pattern
  ) && builtins.all (i:
    builtins.elemAt pattern i == builtins.elemAt path i
  ) (range 0 (builtins.length pattern - 1));

  # Check whether a path matches any exclude pattern.
  isExcluded = excludes: path:
    builtins.any (matchesExclude path) excludes;

  # Recursively walk the flake tree following an include pattern.
  # `node`    — current attrset being traversed
  # `pattern` — remaining segments of the include pattern (list of strings)
  # `prefix`  — segments already traversed (list of strings), used to build the attr path
  # `excludes`— list of exclude patterns
  #
  # When the pattern is exhausted, check if the current node is a derivation.
  # A trailing "*" means: enumerate all attrs, recurse into each, and collect
  # any that are derivations (one level of wildcard also recurses into nested
  # attrsets to handle the collapsed `*.*` → `*` from Wildcard::path_to_nix_list).
  resolve = excludes: node: pattern: prefix: let
    patLen = builtins.length pattern;
  in
    if patLen == 0 then
      # Leaf: check if this node is a derivation and not excluded.
      if safeIsDerivation node && !(isExcluded excludes prefix) then
        [ (builtins.concatStringsSep "." prefix) ]
      else
        [ ]
    else let
      seg = builtins.head pattern;
      rest = builtins.tail pattern;
    in
      if seg == "*" then
        # Recursive wildcard: enumerate all attributes of the current node.
        builtins.concatMap (name: let
          child = safeGetAttr name node;
          newPrefix = prefix ++ [ name ];
        in
          if !child.success then
            [ ]
          else
            if rest == [ ] then (
              # Last segment: check derivation + recurse one level deeper
              # to handle the `*.*` collapse (consecutive `*` segments are
              # collapsed to a single `*` by build_wildcard_nix_expr, so a
              # trailing `*` may represent multiple wildcard levels).
              if safeIsDerivation child.value && !(isExcluded excludes newPrefix) then
                [ (builtins.concatStringsSep "." newPrefix) ]
              else
                builtins.concatMap (subName: let
                  subChild = safeGetAttr subName child.value;
                  subPrefix = newPrefix ++ [ subName ];
                in if !subChild.success then
                  [ ]
                else if safeIsDerivation subChild.value && !(isExcluded excludes subPrefix) then
                  [ (builtins.concatStringsSep "." subPrefix) ]
                else
                  [ ]
              ) (safeAttrNames child.value)
            ) else
              resolve excludes child.value rest newPrefix
        ) (safeAttrNames node)
      else
        if seg == "#" then
          # Non-recursive wildcard: enumerate all attributes of the current node.
          # When `#` is the last segment, collect only children where
          # `type == "derivation"` — no further descent (unlike `*`).
          # When more segments follow (e.g. `#.foo` or `#.#`), recurse into each
          # child with the remaining pattern, preserving the depth-precise semantics.
          builtins.concatMap (name: let
            child = safeGetAttr name node;
            newPrefix = prefix ++ [ name ];
          in
            if !child.success then
              [ ]
            else
              if rest == [ ] then
                if safeIsDerivation child.value && !(isExcluded excludes newPrefix) then
                  [ (builtins.concatStringsSep "." newPrefix) ]
                else
                  [ ]
              else
                resolve excludes child.value rest newPrefix
          ) (safeAttrNames node)
        else
          # Literal segment: descend directly.
          let
            child = safeGetAttr seg node;
          in
            if child.success then
              resolve excludes child.value rest (prefix ++ [ seg ])
            else
              [ ];
in builtins.toJSON (
  builtins.concatMap (pattern:
    resolve wildcard_ref.exclude flake pattern [ ]
  ) wildcard_ref.include
)
