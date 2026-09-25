# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
{ lib, python3Packages }:

let
  # The inspector refuses every report version but the one it pins, and it ships
  # separately from the server that writes them - so a bump that reaches only the
  # exporter turns it into a tool that refuses every real report. That shipped:
  # it sat on 10 while the exporter wrote 11. This is the only place both
  # constants are visible, so the check belongs here, at evaluation.
  constantAfter =
    prefix: path:
    let
      hit = lib.findFirst (l: lib.hasPrefix prefix l) null (
        lib.splitString "\n" (builtins.readFile path)
      );
    in
    if hit == null then
      throw "report-inspector: no line starting with '${prefix}' in ${toString path}"
    else
      lib.head (builtins.match "([0-9]+).*" (lib.removePrefix prefix hit));

  written = constantAfter "pub const SCHEMA_VERSION: i64 = " ../../../backend/gradient-report/src/schema.rs;
  read = constantAfter "SUPPORTED_SCHEMA = " ./gradient_report/db.py;
in

assert lib.assertMsg (read == written) ''
  report-inspector reads report schema ${read}, but gradient-report writes ${written}.
  Set SUPPORTED_SCHEMA in nix/tools/report-inspector/gradient_report/db.py to ${written}.
'';

python3Packages.buildPythonApplication {
  pname = "gradient-report-inspector";
  version = "1.4.0";
  pyproject = true;
  src = ./.;

  build-system = [ python3Packages.setuptools ];

  # Stdlib only, so there is nothing to propagate: the inspector has to run on
  # whatever machine a maintainer opens the report on.
  dependencies = [ ];

  # setuptools' console script only appends its own site-packages, so an ambient
  # PYTHONPATH naming another build of this package wins the import and answers
  # for a schema this one does not read. The devShell exports exactly that, so a
  # shell entered before a bump hijacks every later build, `nix run` included.
  makeWrapperArgs = [ "--unset PYTHONPATH" ];

  nativeCheckInputs = [ python3Packages.pytestCheckHook ];

  meta = {
    description = "Inspect a Gradient evaluation diagnostic report";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    mainProgram = "gradient-report";
  };
}
