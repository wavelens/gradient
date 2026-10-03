/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{
  name = "download";
  derivations = {
    src = { download = true; };
    app = { deps = [ "src" ]; };
  };
  entryPoints = [ "app" ];
}
