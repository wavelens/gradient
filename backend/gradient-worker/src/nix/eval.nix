flake_ref: wildcard_ref:
let
  flake = (builtins.getFlake flake_ref).outputs;

  range = first: last:
    if first > last then [ ]
    else builtins.genList (n: first + n) (last - first + 1);

  safeCall = v: let p = builtins.tryEval v; in if p.success then p.value else null;

  isDrv = v:
    builtins.isAttrs v &&
    (v.type or "" == "derivation" || v ? outPath);

  isOpaque = v:
    builtins.isAttrs v &&
    (v ? type || v ? _type) &&
    !(isDrv v);

  safeAttrNames = v:
    let p = builtins.tryEval (if builtins.isAttrs v then builtins.attrNames v else [ ]);
    in if p.success then p.value else [ ];

  safeGet = name: v: builtins.tryEval (builtins.getAttr name v);

  # --- Matching Logic ---
  matchesExclude = path: pattern:
    (builtins.length path == builtins.length pattern) &&
    builtins.all (i: builtins.elemAt pattern i == builtins.elemAt path i)
      (range 0 (builtins.length pattern - 1));

  isExcluded = excludes: path: builtins.any (matchesExclude path) excludes;

  # --- Main Resolver ---
  resolve = excludes: node: pattern: prefix:
    let
      patLen = builtins.length pattern;
    in
    if patLen == 0 then
      if (safeCall (isDrv node)) && !(isExcluded excludes prefix) then
        [ (builtins.concatStringsSep "." prefix) ]
      else [ ]
    else
      let
        seg = builtins.head pattern;
        rest = builtins.tail pattern;
      in
      if seg == "*" then
        builtins.concatMap (name:
          let
            child = safeGet name node;
            newPrefix = prefix ++ [ name ];
          in
          if !child.success then [ ]
          else if rest == [ ] then
            # Trailing '*' logic: Check self + 1 level deeper (the *.* collapse)
            if (safeCall (isDrv child.value)) && !(isExcluded excludes newPrefix) then
              [ (builtins.concatStringsSep "." newPrefix) ]
            else if (safeCall (isOpaque child.value)) then [ ]
            else
              builtins.concatMap (subName:
                let
                  subChild = safeGet subName child.value;
                  subPrefix = newPrefix ++ [ subName ];
                in
                if !subChild.success then [ ]
                else if (safeCall (isDrv subChild.value)) && !(isExcluded excludes subPrefix) then
                  [ (builtins.concatStringsSep "." subPrefix) ]
                else [ ]
              ) (safeAttrNames child.value)
          else if (safeCall (isOpaque child.value)) then [ ]
          else resolve excludes child.value rest newPrefix
        ) (safeAttrNames node)

      else if seg == "#" then
        builtins.concatMap (name:
          let
            child = safeGet name node;
            newPrefix = prefix ++ [ name ];
          in
          if !child.success then [ ]
          else if rest == [ ] then
            if (safeCall (isDrv child.value)) && !(isExcluded excludes newPrefix) then
              [ (builtins.concatStringsSep "." newPrefix) ]
            else [ ]
          else resolve excludes child.value rest newPrefix
        ) (safeAttrNames node)

      else
        # Literal segment
        let child = safeGet seg node;
        in if child.success then resolve excludes child.value rest (prefix ++ [ seg ]) else [ ];

in builtins.toJSON (
  builtins.concatMap (pattern:
    resolve wildcard_ref.exclude flake pattern [ ]
  ) wildcard_ref.include
)
