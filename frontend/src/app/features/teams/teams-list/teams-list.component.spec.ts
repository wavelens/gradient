/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { TeamsListComponent } from './teams-list.component';
import { TeamsService } from '@core/services/teams.service';
import { ConfigService } from '@core/services/config.service';
import { AuthService } from '@core/services/auth.service';

function setup(canCreateTeam: boolean) {
  const create = vi.fn().mockReturnValue(of('id'));
  TestBed.configureTestingModule({
    imports: [TeamsListComponent],
    providers: [
      provideRouter([]),
      { provide: TeamsService, useValue: { list: () => of([{ name: 'platform', display_name: 'Platform', role: 'admin' }]), create } },
      {
        provide: ConfigService,
        useValue: { createTeam: canCreateTeam ? 'everyone' : 'none', canCreate: (p: string) => p === 'everyone' },
      },
      { provide: AuthService, useValue: { isAuthenticated: () => true, user: () => null } },
    ],
  });
  const fixture = TestBed.createComponent(TeamsListComponent);
  fixture.detectChanges();
  return { fixture, create };
}

describe('TeamsListComponent', () => {
  it('lists the teams of the user with their role', () => {
    const { fixture } = setup(true);
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).toContain('Platform');
    expect(text).toContain('admin');
  });

  it('hides New Team when the server does not allow creating teams', () => {
    const { fixture } = setup(false);
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).not.toContain('New Team');
  });
});
