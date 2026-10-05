/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { EntryPointSummary } from '@core/models/task.model';
import { groupEntryPoints } from './entry-point-groups';

const ep = (attr: string, architecture = 'x86_64-linux') =>
  ({ id: attr, eval: attr, architecture }) as EntryPointSummary;

const shape = (eps: EntryPointSummary[]) =>
  groupEntryPoints(eps).map((g) => [g.title, g.rows.map((r) => r.label)]);

describe('groupEntryPoints', () => {
  it('heads the rows with their attribute set and drops it and the architecture from the labels', () => {
    expect(shape([ep('packages.x86_64-linux.hello'), ep('packages."x86_64-linux"."foo.bar"')]))
      .toEqual([['Packages', ['hello', 'foo.bar']]]);
  });

  it('keeps a second segment that is not the entry point architecture', () => {
    expect(shape([ep('legacyPackages.x86_64-linux.python3Packages.requests'), ep('lib.aarch64-linux.hello')]))
      .toEqual([
        ['LegacyPackages', ['python3Packages.requests']],
        ['Lib', ['aarch64-linux.hello']],
      ]);
  });

  it('gives every attribute set its own heading in the order the rows arrive', () => {
    expect(shape([ep('checks.x86_64-linux.fmt'), ep('packages.x86_64-linux.hello'), ep('checks.x86_64-linux.lint')]))
      .toEqual([
        ['Checks', ['fmt', 'lint']],
        ['Packages', ['hello']],
      ]);
  });

  /// A NixOS flake's entry points all end `.config.system.build.toplevel`, so the
  /// last segment labelled every row of a 74-host list `toplevel`.
  it('drops the trailing segments every row of a set shares', () => {
    const host = (n: string) => ep(`nixosConfigurations.${n}.config.system.build.toplevel`);
    expect(shape([host('broker'), host('caveman')])).toEqual([['NixosConfigurations', ['broker', 'caveman']]]);
  });

  it('never strips a label down to nothing', () => {
    expect(shape([ep('hello'), ep('packages.x86_64-linux.default')]))
      .toEqual([
        ['Hello', ['hello']],
        ['Packages', ['default']],
      ]);
  });
});
