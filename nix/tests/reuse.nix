# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only

{ pkgs, lib }: pkgs.runCommand "lint-reuse-metadata" {} ''
    set -euo pipefail

    cd "${../..}"
    ${lib.getExe pkgs.reuse} lint
    touch "$out"
''
