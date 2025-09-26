{ self, pkgs, depth ? 1, width ? 1, seed ? "42", ... }: let
  inherit (pkgs) lib;
  previons_build_seed = builtins.hashString "md5" (seed + toString depth + toString width);
  previons_build_width = (lib.mod (lib.fromHexString (builtins.substring 0 1 previons_build_seed)) 10) + 1;

  previous_build_test = lib.concatStringsSep "\n" (map (w: "${pkgs.coreutils}/bin/ls ${self.packages.x86_64-linux.build-test {
    seed = previons_build_seed;
    depth = depth - 1;
    width = w;
  }}/data >> $out/previous_tests") (lib.range 1 previons_build_width));
in builtins.derivation {
  name = "build-test";
  system = "x86_64-linux";
  builder = "/bin/sh";
  args = [ "-c" ''
    ${pkgs.coreutils}/bin/mkdir -p $out/data
    ${lib.optionalString (depth > 0) previous_build_test}
    ${pkgs.coreutils}/bin/dd if=/dev/zero of=$out/data/${seed}-${toString width}.bin bs=512k count=1
    echo "Build Test #${toString width} in Layer #${toString depth} generated 512kB file"
  '' ];
}
