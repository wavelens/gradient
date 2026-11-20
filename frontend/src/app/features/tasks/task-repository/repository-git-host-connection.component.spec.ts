/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import { GitHostConnectionSummary, TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { ConfigService } from '@core/services/config.service';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { ApiError } from '@core/services/api.service';
import { RepositoryGitHostConnectionAndCommitStatusComponent } from './repository-git-host-connection.component';
import { chainWithoutConnection } from './task-git-host-chain.testing';

const reporting: GitHostConnectionSummary = {
  id: 'c1',
  owner: { type: 'team', name: 'platform' },
  name: 'forgejo',
  display_name: 'Forgejo',
  git_host_type: 'forgejo',
  base_url: 'https://forgejo.example.com',
  receives_events: true,
  reports_status: true,
  can_manage: false,
  access_token_refreshes_automatically: false,
  refused_signature_count: 0,
  task_count: 0,
  repository_fit: { fits: true },
};

const eventsOnly: GitHostConnectionSummary = {
  ...reporting,
  id: 'c2',
  owner: { type: 'project', name: 'acme' },
  name: 'hooks',
  display_name: 'Hooks',
  reports_status: false,
  can_manage: true,
};

const otherHost: GitHostConnectionSummary = {
  ...reporting,
  id: 'c3',
  name: 'gitlab',
  display_name: 'GitLab',
  base_url: 'https://gitlab.com',
  repository_fit: { fits: false, reason: 'Other host' },
};

const state: TaskRepositoryResponse = {
  repository: 'https://forgejo.example.com/acme/app.git',
  uses_ssh: false,
  deploy_key: null,
  fetch: null,
  git_host_connection: null,
  report_status: false,
  recent_deliveries: [],
  chain: chainWithoutConnection,
  status_posts: [],
  available_connections: [reporting, eventsOnly, otherHost],
  can_edit: true,
  can_manage_integrations: false,
};

function render(overrides: Partial<TaskRepositoryResponse> = {}, tasks: object = {}) {
  const notify = { success: vi.fn(), failure: vi.fn() };
  TestBed.configureTestingModule({
    imports: [RepositoryGitHostConnectionAndCommitStatusComponent],
    providers: [
      provideRouter([]),
      { provide: TasksService, useValue: tasks },
      { provide: NotifyService, useValue: notify },
      { provide: GitHostConnectionsApi, useValue: {} },
      { provide: ConfigService, useValue: { giteaOauthAppConfig: null } },
    ],
  });
  const fixture = TestBed.createComponent(RepositoryGitHostConnectionAndCommitStatusComponent);
  fixture.componentRef.setInput('project', 'acme');
  fixture.componentRef.setInput('task', 'app');
  fixture.componentRef.setInput('state', { ...state, ...overrides });
  fixture.detectChanges();
  const el = fixture.nativeElement as HTMLElement;
  const savedStates: TaskRepositoryResponse[] = [];
  fixture.componentInstance.saved.subscribe((saved) => savedStates.push(saved));
  return { fixture, el, component: fixture.componentInstance, notify, savedStates };
}

function testId(el: HTMLElement, id: string): HTMLElement | null {
  return el.querySelector(`[data-testid="${id}"]`);
}

describe('RepositoryGitHostConnectionAndCommitStatusComponent', () => {
  it('offers only the connections that fit the repository, plus the current one', () => {
    expect(render().component.pickerOptions().map((option) => option.label)).toEqual([
      'No connection',
      'Forgejo · forgejo.example.com · Team platform',
      'Hooks · forgejo.example.com · Project acme',
    ]);
    TestBed.resetTestingModule();
    const current = render({ git_host_connection: otherHost });
    expect(current.component.pickerOptions().map((option) => option.value)).toEqual(['', 'c1', 'c2', 'c3']);
  });

  it('saves the picked connection at once and passes the new state on', () => {
    const saved = { ...state, git_host_connection: reporting };
    const updateTaskRepositoryConnection = vi.fn(() => of(saved));
    const { component, savedStates } = render({}, { updateTaskRepositoryConnection });
    component.selectConnection('c1');
    expect(updateTaskRepositoryConnection).toHaveBeenCalledWith('acme', 'app', { git_host_connection: 'c1' });
    expect(savedStates).toEqual([saved]);
  });

  it('returns the picker to the saved connection when saving fails', () => {
    const updateTaskRepositoryConnection = vi.fn(() => throwError(() => new ApiError('nope', 400)));
    const { component, notify } = render({}, { updateTaskRepositoryConnection });
    component.selectConnection('c1');
    expect(component.selectedId()).toBe('');
    expect(notify.failure).toHaveBeenCalled();
  });

  it('selects the single fitting connection directly', () => {
    const updateTaskRepositoryConnection = vi.fn(() => of(state));
    const { component } = render({ available_connections: [reporting, otherHost] }, { updateTaskRepositoryConnection });
    component.selectFittingConnectionOrFocusPicker();
    expect(updateTaskRepositoryConnection).toHaveBeenCalledWith('acme', 'app', { git_host_connection: 'c1' });
  });

  it('saves report status at once and tests it beside the checkbox', () => {
    const updateTaskRepositoryConnection = vi.fn(() => of({ ...state, git_host_connection: reporting, report_status: true }));
    const postPendingTestCommitStatus = vi.fn(() => of({ success: true, response_status: 201 }));
    const { fixture, el, component, notify } = render({ git_host_connection: reporting }, { updateTaskRepositoryConnection, postPendingTestCommitStatus });
    expect(testId(el, 'test-status-report')).toBeNull();
    component.setReportStatus(true);
    expect(updateTaskRepositoryConnection).toHaveBeenCalledWith('acme', 'app', { report_status: true });
    fixture.componentRef.setInput('state', { ...state, git_host_connection: reporting, report_status: true });
    fixture.detectChanges();
    testId(el, 'test-status-report')!.click();
    expect(postPendingTestCommitStatus).toHaveBeenCalledWith('acme', 'app');
    expect(notify.success).toHaveBeenCalledWith('Status report sent');
  });

  it('links to the connection only where the viewer can open it', () => {
    expect(testId(render({ git_host_connection: eventsOnly }).el, 'open-connection')).not.toBeNull();
    TestBed.resetTestingModule();
    expect(testId(render({ git_host_connection: reporting }).el, 'open-connection')).toBeNull();
  });

  it('offers a new connection in place when none fits, or links to the connections page', () => {
    const manager = render({ available_connections: [otherHost], can_manage_integrations: true });
    expect(testId(manager.el, 'no-connections')?.textContent).toContain('No connection for forgejo.example.com');
    expect(testId(manager.el, 'new-connection')).not.toBeNull();
    TestBed.resetTestingModule();
    const member = render({ available_connections: [] });
    expect(testId(member.el, 'new-connection')).toBeNull();
    expect(testId(member.el, 'no-connections')?.querySelector('a')?.getAttribute('href')).toBe('/project/acme/git-host-connections');
  });

  it('leaves the next action to the chain instead of offering it twice', () => {
    const createConnection = { ...chainWithoutConnection, connection: { ...chainWithoutConnection.connection, action: { type: 'create_connection' as const, git_host_type: 'forgejo', base_url: 'https://forgejo.example.com' } } };
    const manager = render({ available_connections: [], can_manage_integrations: true, chain: createConnection });
    expect(manager.el.querySelector('gr-settings-section')).toBeNull();
    TestBed.resetTestingModule();
    const testReport = { ...chainWithoutConnection, commit_status: { state: 'attention' as const, detail: 'No status posted yet', action: { type: 'test_status_report' as const } } };
    const reporter = render({ git_host_connection: reporting, report_status: true, chain: testReport });
    expect(testId(reporter.el, 'test-status-report')).toBeNull();
  });
});
