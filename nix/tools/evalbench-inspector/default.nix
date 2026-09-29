# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
{ lib, python3Packages }:

python3Packages.buildPythonApplication {
  pname = "gradient-evalbench-inspector";
  version = "1.4.1";
  pyproject = true;
  src = ./.;

  build-system = [ python3Packages.setuptools ];

  # Stdlib only: the report has to render wherever a bundle is downloaded to.
  dependencies = [ ];

  makeWrapperArgs = [ "--unset PYTHONPATH" ];

  nativeCheckInputs = [ python3Packages.pytestCheckHook ];

  meta = {
    description = "Render a Gradient eval benchmark bundle as an inspectable HTML report";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    mainProgram = "gradient-evalbench";
  };
}
