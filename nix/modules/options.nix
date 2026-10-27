/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{
  lib,
  specs,
  subGroups ? { },
  enabledGroups ? [ ],
  groupSwitches ? { },
  writtenByHand ? [ ],
  withoutEnvironment ? [ ],
  examples ? { },
  emptyListIsUnset ? false,
}: let
  camel = name: let
    parts = lib.splitString "_" name;
    capital = part: lib.toUpper (lib.substring 0 1 part) + lib.substring 1 (-1) part;
  in lib.concatStrings ([ (lib.head parts) ] ++ map capital (lib.tail parts));

  pathOf = spec: let
    group = lib.optional (spec.group != "") (camel spec.group);
    subGroup = lib.findFirst (name: lib.hasPrefix "${name}_" spec.field) null (subGroups.${spec.group} or [ ]);
  in if subGroup == null then
    group ++ [ (camel spec.field) ]
  else
    group ++ [ (camel subGroup) (camel (lib.removePrefix "${subGroup}_" spec.field)) ];

  isCredential = spec: lib.hasSuffix "_file" spec.field;
  isOptional = spec: lib.hasPrefix "Option<" spec.type;
  valueType = spec: if isOptional spec then lib.removeSuffix ">" (lib.removePrefix "Option<" spec.type) else spec.type;
  isInteger = spec: lib.elem (valueType spec) [
    "u16" "u32" "u64" "usize" "i32" "i64" "NonZeroU32" "NonZeroU64" "NonZeroUsize"
  ];

  typeOf = spec: let
    name = valueType spec;
    type =
      if name == "bool" then lib.types.bool
      else if spec.values != [ ] then lib.types.enum spec.values
      else if name == "u16" then lib.types.port
      else if lib.hasPrefix "NonZero" name then lib.types.ints.positive
      else if lib.elem name [ "u32" "u64" "usize" ] then lib.types.ints.unsigned
      else if lib.elem name [ "i32" "i64" ] then lib.types.int
      else if name == "PathBuf" then lib.types.path
      else if name == "Vec<String>" then lib.types.listOf lib.types.str
      else lib.types.str;
  in if isOptional spec then lib.types.nullOr type else type;

  defaultOf = spec:
    if spec.type == "bool" then spec.default == "true"
    else if spec.type == "Vec<String>" then lib.optionals (spec.default != null) (lib.splitString "," spec.default)
    else if spec.default == null then null
    else if isInteger spec then lib.toInt spec.default
    else spec.default;

  optionOf = spec: lib.mkOption ({
    type = typeOf spec;
    default = defaultOf spec;
    description = spec.description;
  } // lib.optionalAttrs (spec.type == "bool" && lib.hasPrefix "Whether to enable" spec.description) {
    example = true;
  } // lib.optionalAttrs (lib.hasAttrByPath (pathOf spec) examples) {
    example = lib.getAttrFromPath (pathOf spec) examples;
  });

  environmentValue = value:
    if lib.isBool value then lib.boolToString value
    else if lib.isList value then lib.concatStringsSep "," value
    else toString value;

  isEnabled = cfg: spec:
    (!(lib.elem spec.group enabledGroups)
      || spec.field == "enable"
      || lib.getAttrFromPath [ (camel spec.group) "enable" ] cfg)
    && (!(groupSwitches ? ${spec.group}) || lib.getAttrFromPath groupSwitches.${spec.group} cfg);

  isSet = value: value != null && !(emptyListIsUnset && value == [ ]);
in {
  options = lib.foldl' lib.recursiveUpdate { } (map
    (spec: lib.setAttrByPath (pathOf spec) (optionOf spec))
    (lib.filter (spec: !(isCredential spec) && !(lib.elem (pathOf spec) writtenByHand)) specs));

  environment = cfg: lib.listToAttrs (lib.concatMap (spec: let
    value = lib.getAttrFromPath (pathOf spec) cfg;
  in lib.optional (isEnabled cfg spec && isSet value) (lib.nameValuePair spec.env (environmentValue value)))
    (lib.filter (spec: spec.env != null && !(isCredential spec) && !(lib.elem (pathOf spec) withoutEnvironment)) specs));
}
