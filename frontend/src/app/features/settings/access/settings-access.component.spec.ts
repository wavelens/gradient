/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import type { UserAccessSummary } from '@core/models';
import { ApiError } from '@core/services/api.service';
import { ConfigService } from '@core/services/config.service';
import { PendingInvitesService } from '@core/services/pending-invites.service';
import { UserService } from '@core/services/user.service';
import { SettingsAccessComponent } from './settings-access.component';

const aliceTeam = { name: 'alice-team', display_name: 'Alice Team' };

const summary: UserAccessSummary = {
  superuser: false,
  projects: [
    {
      name: 'acme',
      display_name: 'Acme',
      owner_team: aliceTeam,
      role: 'Write',
      reasons: [
        { kind: 'direct', role: 'Write', applies: true, managed: false },
        { kind: 'owner', team: aliceTeam, role: 'View', applies: false, managed: false },
      ],
    },
  ],
  caches: [{ name: 'main', display_name: 'Main', role: 'View', reasons: [{ kind: 'team', team: aliceTeam, role: 'View', applies: true, managed: false }] }],
  teams: [{ name: 'alice-team', display_name: 'Alice Team', role: 'admin', source: 'state' }],
};

function render(load: () => Observable<UserAccessSummary>) {
  const getAccess = vi.fn(load);
  TestBed.configureTestingModule({
    imports: [SettingsAccessComponent],
    providers: [
      provideRouter([]),
      { provide: UserService, useValue: { getProjectsCachesAndTeamsCurrentUserCanReach: getAccess } },
      { provide: ConfigService, useValue: { sshEnabled: false } },
      { provide: PendingInvitesService, useValue: { count: () => 0 } },
    ],
  });
  const fixture = TestBed.createComponent(SettingsAccessComponent);
  fixture.detectChanges();
  return { fixture, root: fixture.nativeElement as HTMLElement, getAccess };
}

function rowMeta(root: HTMLElement, href: string): string {
  return root.querySelector(`a[href="${href}"]`)?.closest('gr-row')?.querySelector('[slot="meta"]')?.textContent?.trim() ?? '';
}

describe('SettingsAccessComponent', () => {
  it('lists every project, cache and team with the role and where it comes from', () => {
    const { root } = render(() => of(summary));
    expect(rowMeta(root, '/project/acme')).toBe('Role Write · Direct · From Alice Team (owner), overridden by direct role');
    expect(rowMeta(root, '/caches/main')).toBe('Role View · From Alice Team');
    expect(rowMeta(root, '/team/alice-team')).toBe('Role Admin · Direct · server configuration');
    expect(root.querySelector('gr-message-banner')).toBeNull();
  });

  it('says that an instance administrator can open everything', () => {
    const { root } = render(() => of({ ...summary, superuser: true }));
    expect(root.querySelector('gr-message-banner')?.textContent).toContain('This account is an instance administrator');
  });

  it('explains how to get access when the account has none', () => {
    const { root } = render(() => of({ superuser: false, projects: [], caches: [], teams: [] }));
    expect(root.textContent).toContain('No access yet');
  });

  it('shows a failed load with Try Again instead of an empty list', () => {
    const { fixture, root, getAccess } = render(() => throwError(() => new ApiError('Database unavailable', 500)));
    expect(root.textContent).toContain("Couldn't load access");
    expect(root.textContent).not.toContain('No access yet');
    getAccess.mockImplementation(() => of(summary));
    fixture.componentInstance.load();
    fixture.detectChanges();
    expect(rowMeta(root, '/caches/main')).toBe('Role View · From Alice Team');
  });
});
