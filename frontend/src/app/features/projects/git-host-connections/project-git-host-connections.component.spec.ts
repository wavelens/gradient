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
import type { AccessState, GitHostConnectionSummary, ProjectResponse } from '@core/models';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { projectRouteStub } from '@core/resolvers/access-route.testing';
import { ProjectGitHostConnectionsComponent } from './project-git-host-connections.component';

const writable: AccessState = { managed: false, canEdit: true, canTrigger: true };
const readOnly: AccessState = { managed: false, canEdit: false, canTrigger: false };

const ownConnection: GitHostConnectionSummary = {
  id: 'c1',
  owner: { type: 'project', name: 'acme' },
  name: 'forgejo',
  display_name: 'Forgejo',
  git_host_type: 'forgejo',
  base_url: 'https://forgejo.example.com',
  receives_events: true,
  reports_status: true,
  can_manage: true,
};

function setup(access: AccessState, fields: Partial<ProjectResponse> = {}, connections: GitHostConnectionSummary[] = []) {
  const list = vi.fn().mockReturnValue(of(connections));
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

  it('puts New connection in the page header like other create buttons once connections exist', () => {
    const newConnectionButtons = (root: HTMLElement) =>
      Array.from(root.querySelectorAll('button')).filter((button) => button.textContent?.includes('New connection'));
    const filled = setup(writable, {}, [ownConnection]).root;
    const [headerButton] = newConnectionButtons(filled);
    expect(newConnectionButtons(filled)).toHaveLength(1);
    expect(headerButton.closest('app-git-host-connection-list')).toBeNull();
    expect(headerButton.classList).toContain('gr-button--primary');
    TestBed.resetTestingModule();
    expect(newConnectionButtons(setup(writable).root).map((button) => button.closest('app-git-host-connection-list'))).not.toContain(null);
    TestBed.resetTestingModule();
    expect(newConnectionButtons(setup(readOnly, {}, [ownConnection]).root)).toHaveLength(0);
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
