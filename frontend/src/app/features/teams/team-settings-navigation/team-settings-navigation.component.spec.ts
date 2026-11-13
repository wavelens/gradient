/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { TeamSettingsNavigationComponent } from './team-settings-navigation.component';

async function labels(role: 'admin' | 'member', superuser = false) {
  HTMLElement.prototype.scrollIntoView ??= () => {};
  const data = { teamAccess: { team: { name: 'core', role } } };
  TestBed.configureTestingModule({
    providers: [
      provideRouter([]),
      { provide: AuthService, useValue: { user: signal({ superuser }) } },
      { provide: ActivatedRoute, useValue: { snapshot: { data }, data: of(data) } },
    ],
  });
  const fixture = TestBed.createComponent(TeamSettingsNavigationComponent);
  fixture.componentRef.setInput('teamUrlName', 'core');
  fixture.detectChanges();
  await fixture.whenStable();
  return Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.side-navigation-label')).map((label) => label.textContent);
}

describe('TeamSettingsNavigationComponent', () => {
  it.each([
    { viewer: 'team admin', role: 'admin' as const, superuser: false },
    { viewer: 'superuser', role: 'member' as const, superuser: true },
  ])('links event notifications for a $viewer', async ({ role, superuser }) => {
    expect(await labels(role, superuser)).toEqual(['General', 'Members', 'Git host connections', 'Event notifications']);
  });

  it('leaves out event notifications for a plain team member', async () => {
    expect(await labels('member')).toEqual(['General', 'Members', 'Git host connections']);
  });
});
