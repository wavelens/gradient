/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, gettext
, python3
}: let
  python = python3;
  ignoredPaths = [ ".github" "target" ];
in python.pkgs.buildPythonApplication rec {
  pname = "gradient-frontend";
  version = "0.2.0";
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
    django-compression-middleware
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
    sentry-sdk
    uritemplate
    urllib3
    whitenoise
    xstatic-bootstrap
    xstatic-jquery
    xstatic-jquery-ui
  ];

  postBuild = ''
    ${python.pythonOnBuildForHost.interpreter} -OO -m compileall .
    ${python.pythonOnBuildForHost.interpreter} manage.py collectstatic --clear --no-input
    ${python.pythonOnBuildForHost.interpreter} manage.py compilemessages
  '';

  installPhase = let
    pythonPath = python.pkgs.makePythonPath dependencies;
  in ''
    runHook preInstall

    mkdir -p $out/lib/gradient-frontend/static/dashboard
    cp -r {dashboard,static,frontend,locale,manage.py} $out/lib/gradient-frontend
    chmod +x $out/lib/gradient-frontend/manage.py

    makeWrapper $out/lib/gradient-frontend/manage.py $out/bin/gradient-frontend \
      --prefix PYTHONPATH : "${pythonPath}"

    runHook postInstall
  '';

  passthru = {
    inherit python;
  };

  meta = {
    description = "Nix Continuous Integration System Frontend";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    platforms = lib.platforms.unix;
    mainProgram = "gradient-frontend";
  };
}
