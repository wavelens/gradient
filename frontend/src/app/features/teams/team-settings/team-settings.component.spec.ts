/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { TeamSettingsComponent } from './team-settings.component';
import { TeamsService } from '@core/services/teams.service';
import { AuthService } from '@core/services/auth.service';
import { Team } from '@core/models';

const team: Team = {
  id: 't1',
  name: 'platform',
  display_name: 'Platform',
  managed: false,
  role: 'admin',
  oidc_group: null,
  scim_group: null,
  new_project_users: false,
  new_project_workers: false,
  new_project_role: null,
};

function setup(superuser: boolean, managed = false) {
  const update = vi.fn().mockReturnValue(of('Team updated'));
  TestBed.configureTestingModule({
    imports: [TeamSettingsComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ team: 'platform' }) } } },
      { provide: TeamsService, useValue: { get: () => of({ ...team, managed, oidc_group: managed ? 'ci-admins' : null }), update, remove: vi.fn() } },
      { provide: AuthService, useValue: { user: signal({ superuser }) } },
    ],
  });
  const fixture = TestBed.createComponent(TeamSettingsComponent);
  fixture.detectChanges();
  return { fixture, update };
}

function input(fixture: { nativeElement: HTMLElement }, id: string): HTMLInputElement {
  return fixture.nativeElement.querySelector<HTMLInputElement>(`#${id}`)!;
}

describe('TeamSettingsComponent', () => {
  it('shows the settings of a state-managed team read-only', async () => {
    const { fixture } = setup(true, true);
    await fixture.whenStable();
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    expect(element.textContent).toContain('managed by the server configuration');
    expect(input(fixture, 'team-display-name').value).toBe('Platform');
    expect(input(fixture, 'team-display-name').disabled).toBe(true);
    expect(input(fixture, 'team-oidc-group').value).toBe('ci-admins');
    expect(input(fixture, 'team-oidc-group').disabled).toBe(true);
  });

  it('leaves the new project grants to superusers', () => {
    const { fixture, update } = setup(false);
    expect(input(fixture, 'team-oidc-group').disabled).toBe(true);

    const component = fixture.componentInstance;
    component.form = { ...component.form, display_name: 'Platform Team', new_project_workers: true };
    component.save();
    expect(update).toHaveBeenCalledWith('platform', { display_name: 'Platform Team' });
  });

  it('lets a superuser grant the team on every new project', () => {
    const { fixture, update } = setup(true);
    expect(input(fixture, 'team-oidc-group').disabled).toBe(false);

    const component = fixture.componentInstance;
    component.form = { ...component.form, new_project_workers: true };
    component.save();
    expect(update).toHaveBeenCalledWith('platform', { new_project_workers: true });
  });
});
