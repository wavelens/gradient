/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

shell:
let
  capture = ''
    if [ -n "''${NIX_ATTRS_SH_FILE:-}" ]; then . "$NIX_ATTRS_SH_FILE"; fi
    source "$stdenv/setup"
    mkdir -p "$out/bin"
    {
      while IFS= read -r -d "" entry; do
        name="''${entry%%=*}"
        case "$name" in
          HOME|PWD|OLDPWD|SHLVL|TERM|TZ|PATH|_|out|outputs|builder|shell|stdenv) ;;
          TMP|TMPDIR|TEMP|TEMPDIR|NIX_BUILD_TOP|NIX_BUILD_CORES|NIX_LOG_FD|NIX_STORE) ;;
          NIX_ENFORCE_PURITY|NIX_REMOTE|NIX_ATTRS_SH_FILE|NIX_ATTRS_JSON_FILE) ;;
          SSL_CERT_FILE|NIX_SSL_CERT_FILE|SHELLOPTS|BASHOPTS|PPID|UID|EUID) ;;
          *) printf 'export %s=%q\n' "$name" "''${!name}" ;;
        esac
      done < <(env -0)
      printf 'export PATH=%q:"$PATH"\n' "$PATH"
      printf 'export IN_NIX_SHELL=impure\n'
      declare -f
      printf '%s\n' "''${shellHook:-}"
    } > "$out/rc"

    interactive=bash
    IFS=: read -ra dirs <<< "$PATH"
    for dir in "''${dirs[@]}"; do
      case "$dir" in
        */*-bash-interactive-*/bin) if [ -x "$dir/bash" ]; then interactive="$dir/bash"; break; fi ;;
      esac
    done

    printf '#!%s\nexec %q --rcfile %q -i\n' "$BASH" "$interactive" "$out/rc" > "$out/bin/gradient-devshell"
    chmod +x "$out/bin/gradient-devshell"
  '';
  environment = derivation (shell.drvAttrs // {
    name = "${shell.name}-gradient-run";
    outputs = [ "out" ];
    args = [ "-e" "-c" capture ];
  });
in
if shell ? drvAttrs.stdenv then "${environment}/bin/gradient-devshell"
else throw "stdenv doesn't exist in devShell ${shell.name or "<unnamed>"}. gradient run can start only in devShells that mkShell or mkDerivation create."
