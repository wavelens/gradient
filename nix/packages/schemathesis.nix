/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, python3Packages, fetchPypi, rustPlatform, aws-lc, pkg-config }:
let
  fetchWheel = { pname, version, hash }: fetchPypi {
    inherit pname version hash;
    format = "wheel";
    dist = "py3";
    python = "py3";
  };

  jsonschema-rs = python3Packages.jsonschema-rs.overridePythonAttrs (old: rec {
    version = "0.60.3";
    src = fetchPypi {
      inherit version;
      pname = "jsonschema_rs";
      hash = "sha256-HYMdjut3WvVHrsDp912lESKpw1Y0htAuQ9uI1CWzLIY=";
    };
    cargoDeps = rustPlatform.fetchCargoVendor {
      inherit (old) pname;
      inherit version src;
      hash = "sha256-aYfjC21bL4hY/sXdynvOilirdfV1+ms7gy3eg0G25Xs=";
    };
    nativeBuildInputs = old.nativeBuildInputs ++ [ pkg-config ];
    buildInputs = (old.buildInputs or [ ]) ++ [ aws-lc ];
    doCheck = false;
  });

  harfile = python3Packages.buildPythonPackage rec {
    pname = "harfile";
    version = "0.6.0";
    format = "wheel";
    src = fetchWheel {
      inherit pname version;
      hash = "sha256-DyIJ1uwWSirqfo+ytJ2f713e7lG2Xb/SikFnGFw0uzA=";
    };
    pythonImportsCheck = [ "harfile" ];
    meta.license = lib.licenses.mit;
  };

  hypothesis-graphql = python3Packages.buildPythonPackage rec {
    pname = "hypothesis_graphql";
    version = "0.13.6";
    format = "wheel";
    src = fetchWheel {
      inherit pname version;
      hash = "sha256-ZOJCWg7WT88GEK6yoj9h5vpWgknGRjE/OSbiLthGwmk=";
    };
    dependencies = with python3Packages; [ graphql-core hypothesis ];
    pythonImportsCheck = [ "hypothesis_graphql" ];
    meta.license = lib.licenses.mit;
  };
in python3Packages.buildPythonApplication rec {
  pname = "schemathesis";
  version = "4.33.1";
  format = "wheel";

  src = fetchWheel {
    inherit pname version;
    hash = "sha256-QJ7Ko63ofpx6T0F8nvuIvGDnU1JSiB9U8n5HbDS1wYw=";
  };

  dependencies = (with python3Packages; [
    anyio
    click
    hypothesis
    pyrate-limiter
    pytest
    pyyaml
    requests
    rich
    typing-extensions
    werkzeug
  ]) ++ [ harfile hypothesis-graphql jsonschema-rs ];

  pythonImportsCheck = [ "schemathesis" ];

  meta = {
    description = "Property-based testing for web APIs that use OpenAPI and GraphQL schemas";
    homepage = "https://schemathesis.readthedocs.io/";
    license = lib.licenses.mit;
    mainProgram = "st";
  };
}
