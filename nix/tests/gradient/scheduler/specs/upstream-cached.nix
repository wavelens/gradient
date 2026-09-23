/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{
  name = "upstream-cached";
  derivations = {
    lib = { present.cache = true; };
    app = {
      deps = [ "lib" ];
      outputs.out.references = [ "lib.out" ];
    };
  };
  entryPoints = [ "app" ];
}
