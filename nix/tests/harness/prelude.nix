/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, topology }:
''
  WORKER_NODES = [${lib.concatStringsSep ", " topology.workerNodes}]
  PROVIDES = set(${builtins.toJSON topology.provides})


  def requires(what, *tags):
      missing = [tag for tag in tags if tag not in PROVIDES]
      if missing:
          print(f"skipping {what}: topology lacks {', '.join(missing)}")
      return not missing


  ${topology.pythonPrelude}
''
