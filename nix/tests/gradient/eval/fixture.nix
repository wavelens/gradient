{
  description = "gradient eval-worker integration fixture";

  outputs = { self }:
    let
      # Seconds of evaluation, so a second eval of this flake is warm only if it
      # read hello's drvPath back instead of deriving it again.
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

        # Nested attrset: a trailing `*` recovers this one level deeper, `#` must not.
        nested.inner = derivation {
          name = "inner";
          system = "x86_64-linux";
          builder = "/bin/sh";
        };

        # Forces an evaluation error: discovery must skip it and resolve must
        # report it per-item without aborting the rest of the batch (#139).
        boom = throw "boom: this attribute must fail in isolation";
      };
    };
}
