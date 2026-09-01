# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
{ lib, python3Packages }:

python3Packages.buildPythonApplication {
  pname = "gradient-report-inspector";
  version = "1.3.0";
  pyproject = true;
  src = ./.;

  build-system = [ python3Packages.setuptools ];

  # Stdlib only, so there is nothing to propagate: the inspector has to run on
  # whatever machine a maintainer opens the report on.
  dependencies = [ ];

  nativeCheckInputs = [ python3Packages.pytestCheckHook ];

  meta = {
    description = "Inspect a Gradient evaluation diagnostic report";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    mainProgram = "gradient-report";
  };
}
