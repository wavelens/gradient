/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, gettext
, python
}: let
  ignoredPaths = [ ".github" "target" ];
in python.pkgs.buildPythonApplication {
  pname = "gradient-frontend";
  version = "0.1.0";
  pyproject = false;

  src = lib.cleanSourceWith {
    filter = name: type: !(type == "directory" && builtins.elem (baseNameOf name) ignoredPaths);
    src = lib.cleanSource ../../frontend;
  };

  nativeBuildInputs = [
    gettext
  ];

  dependencies = with python.pkgs; [
    bleach
    celery
    channels
    channels-redis
    django
    django-debug-toolbar
    django-parler
    django-redis
    django-rosetta
    django-scheduler
    gunicorn
    mysqlclient
    redis
    requests
    selenium
    uritemplate
    urllib3
    xstatic-bootstrap
    xstatic-jquery
    xstatic-jquery-ui
  ];

  postBuild = ''
    ${python.pythonOnBuildForHost.interpreter} -OO -m compileall src
    ${python.pythonOnBuildForHost.interpreter} src/manage.py collectstatic --clear --no-input
    ${python.pythonOnBuildForHost.interpreter} src/manage.py compilemessages
  '';

  meta = {
    description = "Nix Continuous Integration System Frontend";
    homepage = "https://wavelens.io";
    license = lib.licenses.agpl3Only;
    platforms = lib.platforms.unix;
    mainProgram = "backend";
  };
}
