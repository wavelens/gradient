/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of, throwError } from 'rxjs';
import type { GitHostConnectionOwner, ProjectSummary, RepositoryGrantResponse } from '@core/models';
import { ApiError } from '@core/services/api.service';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { NotifyService } from '@core/services/notify.service';
import { ProjectsService } from '@core/services/projects.service';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { RepositoryGrantsComponent } from './repository-grants.component';

const adminCreatedRepositoryGrant: RepositoryGrantResponse = {
  id: 'grant-1',
  project: 'project-1',
  project_name: 'web',
  repository_owner: 'acme',
  repository_name: 'shop',
  source: 'admin',
  created_by: 'user-1',
  created_at: '2026-01-01T00:00:00',
};

const impliedRepositoryGrantFromStateFile: RepositoryGrantResponse = {
  ...adminCreatedRepositoryGrant,
  id: 'grant-2',
  repository_name: null,
  source: 'state_file',
  created_by: null,
};

const repositoryGrantCreatedByUpgradeMigration: RepositoryGrantResponse = {
  ...adminCreatedRepositoryGrant,
  id: 'grant-3',
  repository_name: 'docs',
  source: 'upgrade',
};

function buildProjectSummaryWithNameAndOwnerTeam(name: string, team: string | null): ProjectSummary {
  return {
    id: `id-${name}`,
    name,
    display_name: name,
    description: '',
    public: false,
    hide_build_requests: false,
    allow_exec_in_build_requests: false,
    managed: false,
    owner_team: team ? { name: team, display_name: team } : null,
    created_by: 'user-1',
    created_at: '2026-01-01T00:00:00',
    running_evaluations: 0,
    waiting_evaluations: 0,
  } as ProjectSummary;
}

const projectSummariesForSelectOptions = [
  buildProjectSummaryWithNameAndOwnerTeam('web', 'platform'),
  buildProjectSummaryWithNameAndOwnerTeam('billing', 'finance'),
  buildProjectSummaryWithNameAndOwnerTeam('personal', null),
];

function renderGrantsComponentWithStubbedServices(
  options: {
    owner?: GitHostConnectionOwner;
    grants?: RepositoryGrantResponse[];
    canAddAndRemoveGrants?: boolean;
    api?: object;
  } = {},
) {
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest) => request.action());
  const getGitHostConnectionRepositoryGrants = vi.fn(() => of(options.grants ?? [adminCreatedRepositoryGrant]));
  const getProjects = vi.fn(() => of({ items: projectSummariesForSelectOptions, total: 3, page: 1, per_page: 100 }));
  TestBed.configureTestingModule({
    imports: [RepositoryGrantsComponent],
    providers: [
      { provide: GitHostConnectionsApi, useValue: { getGitHostConnectionRepositoryGrants, ...options.api } },
      { provide: ProjectsService, useValue: { getProjects } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(RepositoryGrantsComponent);
  fixture.componentRef.setInput('gitHostConnectionOwner', options.owner ?? { type: 'instance' });
  fixture.componentRef.setInput('connectionId', 'connection-1');
  fixture.componentRef.setInput('canAddAndRemoveGrants', options.canAddAndRemoveGrants ?? true);
  fixture.detectChanges();
  const el = fixture.nativeElement as HTMLElement;
  return { fixture, el, component: fixture.componentInstance, notify, confirm, getGitHostConnectionRepositoryGrants, getProjects };
}

describe('RepositoryGrantsComponent', () => {
  it('lists each grant with its project, repository and source', () => {
    const { el, getGitHostConnectionRepositoryGrants } = renderGrantsComponentWithStubbedServices({
      grants: [adminCreatedRepositoryGrant, impliedRepositoryGrantFromStateFile, repositoryGrantCreatedByUpgradeMigration],
    });
    expect(getGitHostConnectionRepositoryGrants).toHaveBeenCalledWith({ type: 'instance' }, 'connection-1');
    const rows = Array.from(el.querySelectorAll('[data-testid="repository-grant"]')).map((row) => row.textContent ?? '');
    expect(rows[0]).toContain('web');
    expect(rows[0]).toContain('acme/shop');
    expect(rows[0]).toContain('Added by admin');
    expect(rows[1]).toContain('All repositories of acme');
    expect(rows[1]).toContain('Grant source: state file');
    expect(rows[2]).toContain('Added by upgrade');
  });

  it('disables the remove control of a state file grant and names the reason', () => {
    const { el } = renderGrantsComponentWithStubbedServices({ grants: [impliedRepositoryGrantFromStateFile] });
    const remove = el.querySelector<HTMLButtonElement>('[data-testid="remove-repository-grant"]');
    expect(remove?.disabled).toBe(true);
    expect(remove?.getAttribute('title')).toContain('state file');
  });

  it('says that no task can use a connection without grants', () => {
    const { el } = renderGrantsComponentWithStubbedServices({ grants: [] });
    expect(el.textContent).toContain('No repository grants');
    expect(el.textContent).toContain('No task can use this connection without a repository grant for the project.');
  });

  it('sends the project and repository and adds the created grant to the list', () => {
    const created: RepositoryGrantResponse = { ...adminCreatedRepositoryGrant, id: 'grant-9', repository_name: 'blog' };
    const addRepositoryGrantToGitHostConnection = vi.fn(() => of(created));
    const { fixture, el, component, notify } = renderGrantsComponentWithStubbedServices({ api: { addRepositoryGrantToGitHostConnection } });
    component.resetGrantFormLoadProjectOptionsAndOpenAddGrantDialog();
    component.patchForm({ project: 'web', repository_owner: ' acme ', repository_name: 'blog' });
    component.sendAddGrantRequest();
    fixture.detectChanges();
    expect(addRepositoryGrantToGitHostConnection).toHaveBeenCalledWith({ type: 'instance' }, 'connection-1', {
      project: 'web',
      repository_owner: 'acme',
      repository_name: 'blog',
    });
    expect(el.querySelectorAll('[data-testid="repository-grant"]')).toHaveLength(2);
    expect(component.addRepositoryGrantDialogOpen()).toBe(false);
    expect(notify.success).toHaveBeenCalled();
  });

  it('Reject an empty repository name for One repository. Show the reason. Send no request.', () => {
    const addRepositoryGrantToGitHostConnection = vi.fn();
    const { fixture, component } = renderGrantsComponentWithStubbedServices({ api: { addRepositoryGrantToGitHostConnection } });
    component.resetGrantFormLoadProjectOptionsAndOpenAddGrantDialog();
    component.patchForm({ project: 'web', repository_owner: 'acme', repository_name: ' ' });
    component.sendAddGrantRequest();
    fixture.detectChanges();
    expect(document.body.textContent).toContain('Enter a repository name or select all repositories of the owner.');
    expect(addRepositoryGrantToGitHostConnection).not.toHaveBeenCalled();
  });

  it('Add a grant to all repositories of the owner only when the selection is explicit. The label of the submit button can show all repositories at that point.', () => {
    const addRepositoryGrantToGitHostConnection = vi.fn(() => of(impliedRepositoryGrantFromStateFile));
    const { fixture, component } = renderGrantsComponentWithStubbedServices({ api: { addRepositoryGrantToGitHostConnection } });
    component.resetGrantFormLoadProjectOptionsAndOpenAddGrantDialog();
    component.patchForm({ project: 'web', repository_owner: 'acme', repository_name: 'shop' });
    fixture.detectChanges();
    Array.from(document.querySelectorAll<HTMLButtonElement>('[role="tab"]'))
      .find((tab) => tab.textContent?.trim() === 'All repositories of the owner')!
      .click();
    fixture.detectChanges();
    const saveRepositoryGrantButton = document.querySelector('[data-testid="save-repository-grant"]');
    expect(saveRepositoryGrantButton?.textContent).toContain('Add grant to all repositories');
    expect(document.querySelector('#grant-repository-name')).toBeNull();
    component.sendAddGrantRequest();
    expect(addRepositoryGrantToGitHostConnection).toHaveBeenCalledWith({ type: 'instance' }, 'connection-1', { project: 'web', repository_owner: 'acme' });
  });

  it('refuses a repository name with a .git ending before any request', () => {
    const addRepositoryGrantToGitHostConnection = vi.fn();
    const { component } = renderGrantsComponentWithStubbedServices({ api: { addRepositoryGrantToGitHostConnection } });
    component.patchForm({ project: 'web', repository_owner: 'acme', repository_name: 'shop.git' });
    component.sendAddGrantRequest();
    expect(component.repositoryNameValidationError()).not.toBeNull();
    expect(addRepositoryGrantToGitHostConnection).not.toHaveBeenCalled();
  });

  it('shows the API message of a 409 in the dialog and keeps it open', () => {
    const addRepositoryGrantToGitHostConnection = vi.fn(() => throwError(() => new ApiError('Repository grant already exists', 409)));
    const { fixture, el, component } = renderGrantsComponentWithStubbedServices({ api: { addRepositoryGrantToGitHostConnection } });
    component.resetGrantFormLoadProjectOptionsAndOpenAddGrantDialog();
    component.patchForm({ project: 'web', repository_owner: 'acme', repository_name: 'shop' });
    component.sendAddGrantRequest();
    fixture.detectChanges();
    expect(component.addGrantFormApiErrorMessage()).toBe('Repository grant already exists');
    expect(component.addRepositoryGrantDialogOpen()).toBe(true);
    expect(el.querySelectorAll('[data-testid="repository-grant"]')).toHaveLength(1);
  });

  it('removes a grant after the confirmation that names the effect on tasks', () => {
    const deleteRepositoryGrantOfGitHostConnection = vi.fn(() => of(true));
    const { fixture, el, component, confirm } = renderGrantsComponentWithStubbedServices({ api: { deleteRepositoryGrantOfGitHostConnection } });
    component.removeGrantAfterConfirmation(adminCreatedRepositoryGrant);
    fixture.detectChanges();
    expect(confirm.mock.calls[0][0].consequence).toBe('Tasks in this project can no longer use connections for this repository.');
    expect(deleteRepositoryGrantOfGitHostConnection).toHaveBeenCalledWith({ type: 'instance' }, 'connection-1', 'grant-1');
    expect(el.querySelectorAll('[data-testid="repository-grant"]')).toHaveLength(0);
  });

  it('shows the grants read only without add or remove controls', () => {
    const { el, getProjects } = renderGrantsComponentWithStubbedServices({ canAddAndRemoveGrants: false });
    expect(el.querySelectorAll('[data-testid="repository-grant"]')).toHaveLength(1);
    expect(el.querySelector('[data-testid="add-repository-grant"]')).toBeNull();
    expect(el.querySelector('[data-testid="remove-repository-grant"]')).toBeNull();
    expect(getProjects).not.toHaveBeenCalled();
  });

  it('offers only projects of the team for a team connection', () => {
    const { component } = renderGrantsComponentWithStubbedServices({ owner: { type: 'team', name: 'platform' } });
    component.resetGrantFormLoadProjectOptionsAndOpenAddGrantDialog();
    expect(component.projectOptionsForGrant().map((option) => option['value'])).toEqual(['web']);
  });

  it('offers every listed project for an instance connection', () => {
    const { component } = renderGrantsComponentWithStubbedServices();
    component.resetGrantFormLoadProjectOptionsAndOpenAddGrantDialog();
    expect(component.projectOptionsForGrant().map((option) => option['value'])).toEqual(['web', 'billing', 'personal']);
  });
});
