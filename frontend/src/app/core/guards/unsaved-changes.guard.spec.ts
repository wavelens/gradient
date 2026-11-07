/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Route } from '@angular/router';
import { routes } from '../../app.routes';
import { unsavedChangesGuard } from './unsaved-changes.guard';

function flatten(list: Route[], prefix = ''): { path: string; route: Route }[] {
  return list.flatMap((route) => {
    const path = [prefix, route.path].filter(Boolean).join('/');
    return [{ path, route }, ...flatten(route.children ?? [], path)];
  });
}

describe('unsavedChangesGuard', () => {
  it.each(['Project Settings', 'Task Settings', 'Cache Settings', 'Team Settings', 'Profile'])(
    'protects the edits on the %s page',
    (title) => {
      const match = flatten(routes).find(({ route }) => route.title === title);
      expect(match, `route titled ${title}`).toBeDefined();
      expect(match!.route.canDeactivate).toContain(unsavedChangesGuard);
    },
  );

  it('asks the page whether it may be left', () => {
    const answer = unsavedChangesGuard({ canDeactivate: () => false }, null as never, null as never, null as never);
    expect(answer).toBe(false);
  });
});
