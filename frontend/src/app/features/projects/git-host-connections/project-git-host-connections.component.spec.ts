/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import type { AccessState, ProjectResponse } from '@core/models';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { projectRouteStub } from '@core/resolvers/access-route.testing';
import { ProjectGitHostConnectionsComponent } from './project-git-host-connections.component';

const writable: AccessState = { managed: false, canEdit: true, canTrigger: true };
const readOnly: AccessState = { managed: false, canEdit: false, canTrigger: false };

function setup(access: AccessState, fields: Partial<ProjectResponse> = {}) {
  const list = vi.fn().mockReturnValue(of([]));
  TestBed.configureTestingModule({
    imports: [ProjectGitHostConnectionsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useFactory: () => projectRouteStub('acme', access, fields) },
      { provide: GitHostConnectionsApi, useValue: { list } },
    ],
  });
  const fixture = TestBed.createComponent(ProjectGitHostConnectionsComponent);
  fixture.detectChanges();
  return { root: fixture.nativeElement as HTMLElement, list };
}

describe('ProjectGitHostConnectionsComponent', () => {
  it('lists the connections of the project for an admin', () => {
    const { root, list } = setup(writable);
    expect(root.querySelector('app-git-host-connection-list')).not.toBeNull();
    expect(list).toHaveBeenCalledWith({ type: 'project', name: 'acme' });
  });

  it('lists the connections read-only for a member without manageIntegrations', () => {
    const { root, list } = setup(readOnly);
    expect(root.querySelector('app-git-host-connection-list')).not.toBeNull();
    expect(list).toHaveBeenCalled();
  });

  it('tells a visitor who is not a member that only members can view this', () => {
    const { root, list } = setup(readOnly, { permissions: [], role: null });
    expect(root.textContent).toContain('Only the members of this project can view this.');
    expect(root.querySelector('app-git-host-connection-list')).toBeNull();
    expect(list).not.toHaveBeenCalled();
  });
});
