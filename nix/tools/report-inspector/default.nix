# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
{ lib, python3Packages }:

let
  # The inspector is refusing every report version except the pinned one.
  # It is released separately from the exporter in the server.
  # A bump reaching only the exporter was already released once (10 vs 11).
  # Both constants are visible only here, making evaluation the place to check them.
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
  version = "2.0.0-rc.1";
  pyproject = true;
  src = ./.;

  build-system = [ python3Packages.setuptools ];

  dependencies = [ ];

  # The setuptools console script is only appending its own site-packages.
  # An ambient PYTHONPATH naming another build is winning the import with the wrong schema.
  # The devShell is exporting exactly that, hijacking every later build including `nix run`.
  makeWrapperArgs = [ "--unset PYTHONPATH" ];

  nativeCheckInputs = [ python3Packages.pytestCheckHook ];

  meta = {
    description = "Inspect a Gradient evaluation diagnostic report";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    mainProgram = "gradient-report";
  };
}
