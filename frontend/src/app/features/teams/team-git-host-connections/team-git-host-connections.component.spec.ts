/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import type { TeamResponse } from '@core/models';
import { AuthService } from '@core/services/auth.service';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { TeamGitHostConnectionsComponent } from './team-git-host-connections.component';

const team: TeamResponse = {
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

function setup(role: TeamResponse['role'], superuser = false) {
  const resolved: TeamResponse = { ...team, role };
  const list = vi.fn().mockReturnValue(of([]));
  TestBed.configureTestingModule({
    imports: [TeamGitHostConnectionsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: { paramMap: convertToParamMap({ team: 'platform' }), data: { teamAccess: { team: resolved } } },
          data: of({ teamAccess: { team: resolved } }),
        },
      },
      { provide: AuthService, useValue: { user: signal({ superuser }) } },
      { provide: GitHostConnectionsApi, useValue: { list } },
    ],
  });
  const fixture = TestBed.createComponent(TeamGitHostConnectionsComponent);
  fixture.detectChanges();
  return { root: fixture.nativeElement as HTMLElement, list };
}

describe('TeamGitHostConnectionsComponent', () => {
  it('lists the connections of the team for a member', () => {
    const { root, list } = setup('admin');
    expect(root.querySelector('app-git-host-connection-list')).not.toBeNull();
    expect(list).toHaveBeenCalledWith({ type: 'team', name: 'platform' });
  });

  it('lists the connections for a superuser who is not a member', () => {
    const { root, list } = setup(null, true);
    expect(root.querySelector('app-git-host-connection-list')).not.toBeNull();
    expect(list).toHaveBeenCalled();
  });

  it('tells a visitor who is not a member that only members can view this', () => {
    const { root, list } = setup(null);
    expect(root.textContent).toContain('Only the members of this team can view this.');
    expect(root.querySelector('app-git-host-connection-list')).toBeNull();
    expect(list).not.toHaveBeenCalled();
  });
});
