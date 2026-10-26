/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

# Twelve independent packages for a split evaluation across the eval workers.
# The task opts in to multi-node evaluation.
{
  name = "split-flake";
  multiNodeEvaluation = true;
  derivations = builtins.listToAttrs (builtins.genList (i: {
    name = "p${toString i}";
    value = { };
  }) 12);
}
