/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Route } from '@angular/router';
import { AuthService } from '@core/services/auth.service';
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

  function leave(signedIn: boolean) {
    TestBed.configureTestingModule({ providers: [{ provide: AuthService, useValue: { isAuthenticated: () => signedIn } }] });
    const canDeactivate = vi.fn(() => false);
    const answer = TestBed.runInInjectionContext(() =>
      unsavedChangesGuard({ canDeactivate }, null as never, null as never, null as never),
    );
    return { answer, canDeactivate };
  }

  it('asks the page whether it may be left', () => {
    expect(leave(true).answer).toBe(false);
  });

  it('lets the page go once the session ended', () => {
    const { answer, canDeactivate } = leave(false);
    expect(answer).toBe(true);
    expect(canDeactivate).not.toHaveBeenCalled();
  });
});
