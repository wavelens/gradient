{ inputs, pkgs, ... }:
let
  inherit (inputs) nixpkgs;
  inherit (nixpkgs) lib;

  map-folder = path: builtins.map (name: path + "/" + name);

  tests-gradient = builtins.attrNames (lib.filterAttrs (_: type: type == "directory") (builtins.readDir ./gradient));
  tests = map-folder "gradient" tests-gradient;
in builtins.listToAttrs (map (test: {
  name =  builtins.replaceStrings [ "/" ] [ "-" ] test;
  value = pkgs.testers.runNixOSTest ./${test};
}) tests)