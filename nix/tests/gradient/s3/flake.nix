{
  description = "gradient S3 upload fixture";

  outputs = { self }: {
    packages.x86_64-linux.hello = derivation {
      name = "s3-hello";
      system = "x86_64-linux";
      builder = "@sh@";
      args = [ "-c" "echo uploaded through a presigned PUT > $out" ];
    };
  };
}
