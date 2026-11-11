/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import { GitHostConnectionSummary, TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ApiError } from '@core/services/api.service';
import { RepositoryGitHostConnectionAndCommitStatusComponent } from './repository-git-host-connection.component';

const reporting: GitHostConnectionSummary = {
  id: 'c1',
  owner: { type: 'team', name: 'platform' },
  name: 'forgejo',
  display_name: 'Forgejo',
  git_host_type: 'forgejo',
  receives_events: true,
  reports_status: true,
  can_manage: false,
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

const state: TaskRepositoryResponse = {
  repository: 'https://forgejo.example.com/acme/app.git',
  uses_ssh: false,
  deploy_key: null,
  fetch: null,
  git_host_connection: null,
  webhook_url: null,
  report_status: false,
  last_event: null,
  status_posts: [],
  available_connections: [reporting, eventsOnly],
  can_edit: true,
  can_manage_integrations: false,
};

function render(overrides: Partial<TaskRepositoryResponse> = {}, tasks: object = {}, superuser = false, githubAppEnabled = false) {
  const notify = { success: vi.fn(), failure: vi.fn() };
  TestBed.configureTestingModule({
    imports: [RepositoryGitHostConnectionAndCommitStatusComponent],
    providers: [
      provideRouter([]),
      { provide: TasksService, useValue: tasks },
      { provide: NotifyService, useValue: notify },
      { provide: AuthService, useValue: { user: signal({ superuser }) } },
    ],
  });
  TestBed.inject(ConfigService).githubAppEnabled = githubAppEnabled;
  const fixture = TestBed.createComponent(RepositoryGitHostConnectionAndCommitStatusComponent);
  fixture.componentRef.setInput('project', 'acme');
  fixture.componentRef.setInput('task', 'app');
  fixture.componentRef.setInput('state', { ...state, ...overrides });
  fixture.detectChanges();
  const el = fixture.nativeElement as HTMLElement;
  return { fixture, el, component: fixture.componentInstance, notify };
}

function testId(el: HTMLElement, id: string): HTMLElement | null {
  return el.querySelector(`[data-testid="${id}"]`);
}

describe('RepositoryGitHostConnectionAndCommitStatusComponent', () => {
  it('labels each connection with its owner, host and capabilities', () => {
    const { component } = render();
    expect(component.gitHostConnectionDropdownOptions().map((option) => option.label)).toEqual([
      'No connection',
      'Forgejo · Team platform · Forgejo · events · status',
      'Hooks · Project acme · Forgejo · events',
    ]);
  });

  it('lets a writer without the permission to manage git host connections save the chosen connection with commit status', () => {
    const saved: TaskRepositoryResponse = { ...state, git_host_connection: reporting, report_status: true };
    const updateTaskRepositoryConnection = vi.fn(() => of(saved));
    const { fixture, el, component, notify } = render({}, { updateTaskRepositoryConnection });
    const emitted = vi.fn();
    component.taskRepositoryResponseAfterGitHostSave.subscribe(emitted);

    component.selectGitHostConnectionAndUpdateReportCommitStatus('c1');
    component.reportCommitStatusChecked.set(true);
    fixture.detectChanges();
    (testId(el, 'save-repository-connection') as HTMLButtonElement).click();

    expect(updateTaskRepositoryConnection).toHaveBeenCalledWith('acme', 'app', { git_host_connection: 'c1', report_status: true });
    expect(notify.success).toHaveBeenCalledWith('Repository connection saved');
    expect(emitted).toHaveBeenCalledWith(saved);
  });

  it('turns commit status off and blocks the switch for a connection that cannot report status', async () => {
    const { fixture, el, component } = render({ git_host_connection: reporting, report_status: true });
    component.selectGitHostConnectionAndUpdateReportCommitStatus('c2');
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(component.reportCommitStatusChecked()).toBe(false);
    expect(el.querySelector<HTMLInputElement>('#task-report-commit-status')?.disabled).toBe(true);
    expect(el.textContent).toContain("The selected connection can't report status.");
  });

  it('reports a refused save as a toast with the reason from the server', () => {
    const failure = new ApiError('The connection is not available to this project', 400);
    const { component, notify } = render({}, { updateTaskRepositoryConnection: vi.fn(() => throwError(() => failure)) });
    component.selectGitHostConnectionAndUpdateReportCommitStatus('c1');
    component.save();
    expect(notify.failure).toHaveBeenCalledWith("Couldn't save repository connection", failure);
    expect(component.saving()).toBe(false);
  });

  it('sends a test status report only for a saved connection with reporting on', () => {
    const postPendingTestCommitStatus = vi.fn(() =>
      of({ id: 'p1', event: 'evaluation.queued', success: true, response_status: 201, delivered_at: '2026-01-01T10:00:00Z' }));
    const { fixture, el, component } = render({ git_host_connection: reporting, report_status: true }, { postPendingTestCommitStatus });
    const changed = vi.fn();
    component.changed.subscribe(changed);

    component.reportCommitStatusChecked.set(false);
    expect(component.testStatusReportButtonDisabledReason()).toBe('Save the connection before the test.');
    component.reportCommitStatusChecked.set(true);
    expect(component.testStatusReportButtonDisabledReason()).toBeNull();

    (testId(el, 'test-status-report') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(postPendingTestCommitStatus).toHaveBeenCalledWith('acme', 'app');
    expect(testId(el, 'test-status-report-result')?.textContent?.trim()).toBe('Status report sent · HTTP 201');
    expect(changed).toHaveBeenCalled();
  });

  it('asks to turn reporting on before a test status report', () => {
    const { component } = render({ git_host_connection: eventsOnly });
    expect(component.testStatusReportButtonDisabledReason()).toBe('Turn on reporting of commit status and save first.');
  });

  it('reports a test status report the server refuses as a toast', () => {
    const failure = new ApiError('No commit to report on yet', 409);
    const { component, notify } = render(
      { git_host_connection: reporting, report_status: true },
      { postPendingTestCommitStatus: vi.fn(() => throwError(() => failure)) },
    );
    component.postPendingTestCommitStatusAndShowResultOnTestStatusReportButtonClick();
    expect(notify.failure).toHaveBeenCalledWith('Could not test the status report', failure);
  });

  it('lists the status posts of the task with outcome and HTTP status', () => {
    const { el } = render({
      git_host_connection: reporting,
      report_status: true,
      status_posts: [
        { id: 'p1', event: 'evaluation.completed', success: true, response_status: 201, delivered_at: '2026-01-01T10:00:00Z' },
        { id: 'p2', event: 'evaluation.failed', success: false, response_status: 401, error_message: 'Bad credentials', delivered_at: '2026-01-01T09:00:00Z' },
      ],
    });
    const rows = Array.from(el.querySelectorAll('[data-testid="status-posts"] li'));
    expect(rows).toHaveLength(2);
    expect(rows[0].querySelector('gr-badge .badge')?.className).toContain('is-success');
    expect(rows[0].textContent).toContain('HTTP 201');
    expect(rows[1].querySelector('gr-badge .badge')?.className).toContain('is-danger');
    expect(rows[1].textContent).toContain('Bad credentials');
  });

  it('says so when no status posts exist yet', () => {
    const { el } = render({ git_host_connection: reporting });
    expect(testId(el, 'no-status-posts')?.textContent).toContain('No status posts yet.');
  });

  it('points to the connection page or a project member when no connection is available', () => {
    const manager = render({ available_connections: [], can_manage_integrations: true }).el;
    expect(testId(manager, 'no-connections')?.querySelector('a')?.getAttribute('href')).toBe('/project/acme/git-host-connections');
    TestBed.resetTestingModule();
    const writer = render({ available_connections: [] }).el;
    expect(testId(writer, 'no-connections')?.textContent).toContain('Ask a project member with Write or Admin to create a connection.');
  });

  it('shows read-only members the choice without a way to save it', async () => {
    const { fixture, el } = render({ git_host_connection: reporting, report_status: true, can_edit: false });
    await fixture.whenStable();
    fixture.detectChanges();
    expect(testId(el, 'save-repository-connection')).toBeNull();
    expect(testId(el, 'test-status-report')).toBeNull();
    expect(el.querySelector<HTMLInputElement>('#task-report-commit-status')?.disabled).toBe(true);
  });

  it('leads a superuser to the instance git hosts for a GitHub repository without a GitHub App', () => {
    const { el } = render({ repository: 'https://github.com/acme/app.git' }, {}, true);
    expect(testId(el, 'github-hint')?.querySelector('a')?.getAttribute('href')).toBe('/admin/git-hosts');
  });

  it('tells how to connect a GitHub repository once the instance has a GitHub App', () => {
    const { el } = render({ repository: 'https://github.com/acme/app.git' }, {}, false, true);
    expect(testId(el, 'github-hint')?.textContent).toContain('Install the GitHub App on the GitHub account for this repository.');
  });

  it('shows the webhook URL and installs the webhook through a saved connection that reports status', () => {
    const installWebhook = vi.fn(() => of('ok'));
    const { el, notify } = render(
      { git_host_connection: reporting, report_status: true, webhook_url: 'https://ci.example.com/hooks/c1', can_manage_integrations: true },
      { installWebhook },
    );
    const urls = Array.from(el.querySelectorAll<HTMLInputElement>('gr-copy-field input')).map((input) => input.value);
    expect(urls).toContain('https://ci.example.com/hooks/c1');
    (testId(el, 'install-webhook') as HTMLButtonElement).click();
    expect(installWebhook).toHaveBeenCalledWith('acme', 'app');
    expect(notify.success).toHaveBeenCalledWith('Webhook installed');
  });

  it('asks before leaving with an unsaved connection choice', () => {
    const { component } = render();
    expect(component.unsaved.changed()).toBe(false);
    component.selectGitHostConnectionAndUpdateReportCommitStatus('c1');
    expect(component.unsaved.changed()).toBe(true);
  });
});
