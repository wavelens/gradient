/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{
  name = "cross-worker";
  derivations = {
    dep = { requiredSystemFeatures = [ "feature-a" ]; };
    top = {
      deps = [ "dep" ];
      requiredSystemFeatures = [ "feature-b" ];
      outputs.out.references = [ "dep.out" ];
    };
  };
  entryPoints = [ "top" ];
}
