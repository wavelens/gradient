/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { By } from '@angular/platform-browser';
import { CreateTeamDialogComponent } from '@shared/forms';
import { Observable, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { TeamSummary } from '@core/models';
import { TeamsListComponent } from './teams-list.component';
import { TeamsService } from '@core/services/teams.service';
import { ConfigService } from '@core/services/config.service';
import { AuthService } from '@core/services/auth.service';

const TEAMS = [{ name: 'platform', display_name: 'Platform', role: 'admin' }] as TeamSummary[];

function setup(canCreateTeam: boolean, list: () => Observable<TeamSummary[]> = () => of(TEAMS)) {
  const create = vi.fn().mockReturnValue(of('id'));
  TestBed.configureTestingModule({
    imports: [TeamsListComponent],
    providers: [
      provideRouter([]),
      { provide: TeamsService, useValue: { list, create } },
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

  it('shows a failed load with Try Again instead of the empty state', () => {
    const list = vi.fn().mockReturnValueOnce(throwError(() => new ApiError('database is down', 500))).mockReturnValue(of(TEAMS));
    const { fixture } = setup(true, list);
    const element = fixture.nativeElement as HTMLElement;
    expect(element.textContent).toContain('Failed to load teams');
    expect(element.textContent).toContain('database is down');
    expect(element.textContent).not.toContain('No teams');
    element.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(list).toHaveBeenCalledTimes(2);
    expect(element.textContent).toContain('Platform');
  });

  it('opens the created team', () => {
    const { fixture } = setup(true);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    fixture.componentInstance.openCreate();
    fixture.detectChanges();
    fixture.debugElement.query(By.directive(CreateTeamDialogComponent)).componentInstance.created.emit('ops');
    expect(String(navigate.mock.calls[0][0])).toBe('/team/ops');
  });

  it('hides Create Team when the server does not allow creating teams', () => {
    const { fixture } = setup(false);
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).not.toContain('Create Team');
  });
});
