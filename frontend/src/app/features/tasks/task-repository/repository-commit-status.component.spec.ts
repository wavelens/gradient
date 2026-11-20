/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { GitHostConnectionSummary, TaskCommitStatusPostAttempt, TaskGitHostChain, TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { RepositoryCommitStatusComponent } from './repository-commit-status.component';

const connection: GitHostConnectionSummary = {
  id: 'c1',
  owner: { type: 'project', name: 'acme' },
  name: 'forgejo',
  display_name: 'Forgejo',
  git_host_type: 'forgejo',
  base_url: 'https://forgejo.example.com',
  receives_events: true,
  reports_status: true,
  can_manage: true,
  access_token_refreshes_automatically: false,
  refused_signature_count: 0,
  task_count: 1,
};

const linked: TaskGitHostChain = {
  repository: { state: 'ok', detail: 'acme/app on forgejo.example.com' },
  connection: { state: 'ok', detail: 'Forgejo' },
  incoming_events: { state: 'ok', detail: 'push event queued an evaluation' },
  commit_status: { state: 'off', detail: 'Off for this task', action: { type: 'turn_on_report_status' } },
};

const post = (at: string, success: boolean): TaskCommitStatusPostAttempt => ({
  id: at,
  event: 'push',
  success,
  response_status: success ? 201 : 500,
  delivered_at: at,
});

const state: TaskRepositoryResponse = {
  repository: 'https://forgejo.example.com/acme/app.git',
  uses_ssh: false,
  deploy_key: null,
  fetch: null,
  git_host_connection: connection,
  report_status: false,
  recent_deliveries: [],
  chain: linked,
  status_posts: [],
  available_connections: [connection],
  can_edit: true,
  can_manage_integrations: true,
};

function render(overrides: Partial<TaskRepositoryResponse> = {}, tasks: object = {}, hasPrimaryAction = false) {
  const notify = { success: vi.fn(), failure: vi.fn() };
  TestBed.configureTestingModule({
    imports: [RepositoryCommitStatusComponent],
    providers: [
      { provide: TasksService, useValue: tasks },
      { provide: NotifyService, useValue: notify },
    ],
  });
  const fixture = TestBed.createComponent(RepositoryCommitStatusComponent);
  fixture.componentRef.setInput('project', 'acme');
  fixture.componentRef.setInput('task', 'app');
  fixture.componentRef.setInput('state', { ...state, ...overrides });
  fixture.componentRef.setInput('hasPrimaryAction', hasPrimaryAction);
  fixture.detectChanges();
  return { fixture, el: fixture.nativeElement as HTMLElement, component: fixture.componentInstance, notify };
}

describe('RepositoryCommitStatusComponent', () => {
  it('heads the section with the commit status step and its state', () => {
    const { el } = render();
    expect(el.querySelector('h2')?.textContent).toBe('Commit status');
    expect(el.querySelector('.settings-section__heading app-chain-step-state-badge')?.textContent).toContain('Off');
  });

  it('shows only its state while the connection step blocks it', () => {
    const chain = { ...linked, connection: { state: 'attention' as const, detail: 'No connection' }, commit_status: { state: 'off' as const, detail: 'Connection required' } };
    const { el } = render({ chain, git_host_connection: null });
    expect(el.querySelector('.settings-section__body')?.textContent?.trim()).toBe('Connection required');
    expect(el.querySelector('gr-checkbox')).toBeNull();
  });

  it("saves the 'Report commit status' checkbox at once and emits the new state to the parent", () => {
    const saved = { ...state, report_status: true };
    const updateTaskRepositoryConnection = vi.fn(() => of(saved));
    const { component } = render({}, { updateTaskRepositoryConnection });
    const savedStates: TaskRepositoryResponse[] = [];
    component.saved.subscribe((value) => savedStates.push(value));
    component.setReportStatus(true);
    expect(updateTaskRepositoryConnection).toHaveBeenCalledWith('acme', 'app', { report_status: true });
    expect(savedStates).toEqual([saved]);
  });

  it('leaves the connection field out of the request body, so the server can keep the current connection', () => {
    const updateTaskRepositoryConnection = vi.fn((_project: string, _task: string, _body: object) => of(state));
    const { component } = render({ report_status: true }, { updateTaskRepositoryConnection });
    component.setReportStatus(false);
    expect(Object.keys(updateTaskRepositoryConnection.mock.calls[0][2])).toEqual(['report_status']);
  });

  it('makes the status report test the primary button when no status was posted yet', () => {
    const chain = { ...linked, commit_status: { state: 'attention' as const, detail: 'No commit status posted yet', action: { type: 'test_status_report' as const } } };
    const postPendingTestCommitStatus = vi.fn(() => of({ success: true, response_status: 201 }));
    const { el, component, notify } = render({ chain, report_status: true }, { postPendingTestCommitStatus }, true);
    const changed = vi.fn();
    component.changed.subscribe(changed);
    const test = el.querySelector('[data-testid="test-status-report"]') as HTMLElement;
    expect(test.classList).toContain('gr-button--primary');
    test.click();
    expect(postPendingTestCommitStatus).toHaveBeenCalledWith('acme', 'app');
    expect(notify.success).toHaveBeenCalledWith('Status report sent');
    expect(changed).toHaveBeenCalled();
  });

  it('shows the latest status post with its time and lists earlier posts below', () => {
    const chain = { ...linked, commit_status: { state: 'ok' as const, detail: 'Last posted push status', at: '2026-11-20T10:00:00' } };
    const status_posts = [post('2026-11-20T10:00:00', true), post('2026-11-20T09:00:00', false)];
    const { el } = render({ chain, report_status: true, status_posts });
    expect(el.querySelector('[data-testid="step-status"]')?.textContent).toContain('Last posted push status');
    const history = Array.from(el.querySelectorAll('app-step-history-list li'));
    expect(history).toHaveLength(1);
    expect(history[0].textContent).toContain('Failed with HTTP 500');
  });
});
