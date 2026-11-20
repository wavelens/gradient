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
import { RepositoryGitHostConnectionComponent } from './repository-git-host-connection.component';
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
  repository_fit: { fits: true, can_be_selected: true },
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
  repository_fit: { fits: false, can_be_selected: false, reason: 'Other host' },
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

function render(overrides: Partial<TaskRepositoryResponse> = {}, tasks: object = {}, installUrl: string | null = null) {
  const notify = { success: vi.fn(), failure: vi.fn() };
  TestBed.configureTestingModule({
    imports: [RepositoryGitHostConnectionComponent],
    providers: [
      provideRouter([]),
      { provide: TasksService, useValue: tasks },
      { provide: NotifyService, useValue: notify },
      { provide: GitHostConnectionsApi, useValue: {} },
      { provide: ConfigService, useValue: { giteaOauthAppConfig: null, githubAppInstallUrl: installUrl } },
    ],
  });
  const fixture = TestBed.createComponent(RepositoryGitHostConnectionComponent);
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

describe('RepositoryGitHostConnectionComponent', () => {
  it('shows the section heading with the Connection step and its state badge', () => {
    const { el } = render();
    expect(el.querySelector('h2')?.textContent).toBe('Connection');
    expect(el.querySelector('.settings-section__heading app-chain-step-state-badge')?.textContent).toContain('Incomplete');
  });

  it('offers only connections that fit the repository host plus the currently selected connection', () => {
    expect(render().component.pickerOptions().map((option) => option.label)).toEqual([
      'No connection',
      'Forgejo · forgejo.example.com · Team platform',
      'Hooks · forgejo.example.com · Project acme',
    ]);
    TestBed.resetTestingModule();
    const current = render({ git_host_connection: otherHost });
    expect(current.component.pickerOptions().map((option) => option.value)).toEqual(['', 'c1', 'c2', 'c3']);
  });

  it('lists a connection without host URL after the fitting ones, marked and never offers it as the fitting one', () => {
    const withoutHostUrl: GitHostConnectionSummary = {
      ...reporting,
      id: 'c4',
      display_name: 'Forge',
      base_url: null,
      repository_fit: { fits: false, can_be_selected: true, reason: 'No host URL' },
    };
    const chain = { ...chainWithoutConnection, connection: { state: 'attention' as const, detail: 'Forgejo fits', action: { type: 'select_connection' as const } } };
    const { component } = render({ available_connections: [withoutHostUrl, reporting], chain });
    expect(component.pickerOptions().map((option) => option.label)).toEqual([
      'No connection',
      'Forgejo · forgejo.example.com · Team platform',
      'Forge · Forgejo · Team platform · No host URL',
    ]);
    expect(component.onlyFittingConnection()?.id).toBe('c1');
  });

  it('saves the picked connection at once and passes the new state on', () => {
    const saved = { ...state, git_host_connection: reporting };
    const updateTaskRepositoryConnection = vi.fn(() => of(saved));
    const { component, savedStates } = render({}, { updateTaskRepositoryConnection });
    component.selectConnection('c1');
    expect(updateTaskRepositoryConnection).toHaveBeenCalledWith('acme', 'app', { git_host_connection: 'c1' });
    expect(savedStates).toEqual([saved]);
  });

  it('omits report_status from the body so the server can keep the current value', () => {
    const updateTaskRepositoryConnection = vi.fn((_project: string, _task: string, _body: object) => of(state));
    const { component } = render({ report_status: true }, { updateTaskRepositoryConnection });
    component.selectConnection('c1');
    expect(Object.keys(updateTaskRepositoryConnection.mock.calls[0][2])).toEqual(['git_host_connection']);
  });

  it('sends null to remove the connection', () => {
    const updateTaskRepositoryConnection = vi.fn(() => of(state));
    const { component } = render({ git_host_connection: reporting }, { updateTaskRepositoryConnection });
    component.selectConnection(null);
    expect(updateTaskRepositoryConnection).toHaveBeenCalledWith('acme', 'app', { git_host_connection: null });
  });

  it('resets picker to saved connection after failed save', () => {
    const updateTaskRepositoryConnection = vi.fn(() => throwError(() => new ApiError('nope', 400)));
    const { component, notify } = render({}, { updateTaskRepositoryConnection });
    component.selectConnection('c1');
    expect(component.selectedId()).toBe('');
    expect(notify.failure).toHaveBeenCalled();
  });

  it('selects the only matching connection with the primary button', () => {
    const updateTaskRepositoryConnection = vi.fn(() => of(state));
    const chain = { ...chainWithoutConnection, connection: { state: 'attention' as const, detail: 'Forgejo fits', action: { type: 'select_connection' as const } } };
    const { fixture, el } = render({ available_connections: [reporting, otherHost], chain }, { updateTaskRepositoryConnection });
    fixture.componentRef.setInput('hasPrimaryAction', true);
    fixture.detectChanges();
    const use = testId(el, 'use-fitting-connection')!;
    expect(use.classList).toContain('gr-button--primary');
    use.click();
    expect(updateTaskRepositoryConnection).toHaveBeenCalledWith('acme', 'app', { git_host_connection: 'c1' });
  });

  it('links to the connection only where the viewer can open it', () => {
    expect(testId(render({ git_host_connection: eventsOnly }).el, 'open-connection')).not.toBeNull();
    TestBed.resetTestingModule();
    expect(testId(render({ git_host_connection: reporting }).el, 'open-connection')).toBeNull();
  });

  it('provides a New connection inline or links to the connections page when no matching connection exists', () => {
    const createConnection = { ...chainWithoutConnection, connection: { ...chainWithoutConnection.connection, action: { type: 'create_connection' as const, git_host_type: 'forgejo', base_url: 'https://forgejo.example.com' } } };
    const manager = render({ available_connections: [otherHost], can_manage_integrations: true, can_create_connection: true, chain: createConnection });
    manager.fixture.componentRef.setInput('hasPrimaryAction', true);
    manager.fixture.detectChanges();
    expect(testId(manager.el, 'no-connections')?.textContent).toContain('No connection to forgejo.example.com');
    expect(testId(manager.el, 'new-connection')?.classList).toContain('gr-button--primary');
    TestBed.resetTestingModule();
    const member = render({ available_connections: [] });
    expect(testId(member.el, 'new-connection')).toBeNull();
    expect(testId(member.el, 'no-connections')?.querySelector('a')?.getAttribute('href')).toBe('/project/acme/git-host-connections');
  });

  it('links to the connections page for a task of declarative state even for a manager of integrations', () => {
    const { el } = render({ available_connections: [], can_manage_integrations: true, can_create_connection: false });
    expect(testId(el, 'new-connection')).toBeNull();
    expect(testId(el, 'no-connections')?.querySelector('a')).not.toBeNull();
  });

  it('shows only its state while the repository step blocks it', () => {
    const chain = { ...chainWithoutConnection, connection: { state: 'off' as const, detail: 'Readable repository URL required' } };
    const { el } = render({ chain });
    expect(el.querySelector('.settings-section__body')?.textContent?.trim()).toBe('Readable repository URL required');
    expect(el.querySelector('gr-select')).toBeNull();
  });

  it('offers to install the GitHub App on the account instead of a new connection and reloads after the user returns', () => {
    const install = {
      ...chainWithoutConnection,
      connection: { state: 'attention' as const, detail: 'GitHub App not installed (acme)', action: { type: 'install_github_app' as const, account: 'acme' } },
    };
    const { fixture, el, component } = render(
      { repository: 'https://github.com/acme/app', available_connections: [], can_create_connection: true, chain: install },
      {},
      'https://github.com/apps/gradient-ci/installations/new',
    );
    fixture.componentRef.setInput('hasPrimaryAction', true);
    fixture.detectChanges();
    const link = testId(el, 'github-app-install')!;
    expect(link.getAttribute('href')).toBe('https://github.com/apps/gradient-ci/installations/new');
    expect(link.classList).toContain('gr-button--primary');
    expect(testId(el, 'new-connection')).toBeNull();
    const changes: unknown[] = [];
    component.changed.subscribe(() => changes.push(true));
    link.addEventListener('click', (event) => event.preventDefault());
    link.click();
    window.dispatchEvent(new Event('focus'));
    expect(changes).toHaveLength(1);
  });
});
