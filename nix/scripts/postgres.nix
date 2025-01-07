/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, stdenv, ... }: let
  psql = pkgs.postgresql_17;
in
stdenv.mkDerivation {
  pname = "start-postgres";
  version = "0";

  src = pkgs.fetchurl {
    executable = true;
    url = "https://raw.githubusercontent.com/sapcc/keppel/refs/heads/master/testing/with-postgres-db.sh";
    hash = "sha256-zso9toCgwDwkzHga01gEYPCNYg6GeAp4JcdnmPBgopM=";
  };

  dontUnpack = true;

  installPhase = ''
    mkdir -p $out/bin
    cp $src $out/bin/start-postgres

    substituteInPlace $out/bin/start-postgres --replace pg_ctl ${psql}/bin/pg_ctl
    substituteInPlace $out/bin/start-postgres --replace initdb ${psql}/bin/initdb

    cat $out/bin/start-postgres

    chmod +x $out/bin/start-postgres
  '';

  buildInputs = [ pkgs.bash psql ];

}
