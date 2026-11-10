/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import { Router } from '@angular/router';
import { TasksService } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { TriggersService } from '@core/services/triggers.service';
import { ConfigService } from '@core/services/config.service';
import { ApiError } from '@core/services/api.service';
import { ConnectRepositoryComponent, buildTaskCreationTriggersFromWizardChoices } from './connect-repository.component';

const inbound = { id: 'in-1', name: 'forgejo', display_name: 'forgejo.example.com', kind: 'inbound', git_host_type: 'forgejo' };
const pushSuggestion = { type: 'reporter_push' as const, integration_id: 'in-1' };
const pullRequestSuggestion = { type: 'reporter_pull_request' as const, integration_id: 'in-1' };

function render(query: Record<string, string>, tasks: object, projects: object = {}, permissions: string[] = ['createTask', 'editTask'], suggestions: unknown[] = [pushSuggestion, pullRequestSuggestion]) {
  const data = { projectAccess: { project: { managed: false, permissions } } };
  TestBed.configureTestingModule({
    imports: [ConnectRepositoryComponent],
    providers: [
      provideRouter([]),
      {
        provide: ActivatedRoute,
        useValue: { data: of(data), snapshot: { data, paramMap: convertToParamMap({ project: 'acme' }), queryParamMap: convertToParamMap(query) } },
      },
      { provide: TasksService, useValue: { checkTaskNameAvailable: () => of(true), ...tasks } },
      { provide: ProjectsService, useValue: { getSubscribedCaches: () => of([]), getProject: () => of({ display_name: 'Acme' }), getSSHKey: () => of('ssh-ed25519 AAAA'), ...projects } },
      { provide: IntegrationsService, useValue: { listProjectIntegrationSummaries: () => of([inbound]) } },
      { provide: ConfigService, useValue: { execEnabled: false } },
      { provide: TriggersService, useValue: { getSuggestedPushAndPullRequestTriggerConfigs: vi.fn(() => of(suggestions)) } },
    ],
  });
  const fixture = TestBed.createComponent(ConnectRepositoryComponent);
  fixture.detectChanges();
  return fixture;
}

describe('ConnectRepositoryComponent', () => {
  it('creates nothing before the last step and then creates the task with exactly the chosen triggers', () => {
    const createTask = vi.fn(() => of({ id: 't1', name: 'my-app-1', url: '' }));
    const testRepositoryCloneAccess = vi.fn(() => of('abc123'));
    const fixture = render({}, { createTask, testRepositoryCloneAccess });
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    const comp = fixture.componentInstance;
    comp.repositoryChanged('ssh://git@forgejo.example.com/acme/my-app.git');
    expect(comp.form.display_name).toBe('my-app');
    expect(comp.form.name).toBe('my-app');
    comp.form.branch = 'release';
    comp.continueFromRepository();
    expect(comp.step()).toBe('access');
    expect(comp.deployKey()).toBe('ssh-ed25519 AAAA');
    comp.testAccess();
    expect(testRepositoryCloneAccess).toHaveBeenCalledWith('acme', 'ssh://git@forgejo.example.com/acme/my-app.git');
    expect(comp.repositoryAccessTestPassedForCurrentUrl()).toBe(true);
    comp.continueToTriggers();
    fixture.detectChanges();
    expect(createTask).not.toHaveBeenCalled();

    const el = fixture.nativeElement as HTMLElement;
    expect(el.textContent).toContain('Build pushes from forgejo.example.com');
    expect(comp.suggestions().every((choice) => !choice.selected)).toBe(true);
    comp.toggleSuggestion(0, true);
    comp.form.cron = '0 2 * * *';
    comp.createTaskWithTriggersAndOpenTask();
    expect(createTask).toHaveBeenCalledWith('acme', expect.objectContaining({
      name: 'my-app',
      repository: 'ssh://git@forgejo.example.com/acme/my-app.git',
      exec_enabled: false,
      triggers: [
        { type: 'polling', interval_secs: 300, branch: 'release' },
        { type: 'reporter_push', integration_id: 'in-1', branches: ['release'] },
        { type: 'time', cron: '0 2 * * *', branch: 'release' },
      ],
    }));
    expect(String(navigate.mock.calls.at(-1)?.[0])).toBe('/project/acme/task/my-app-1');
  });

  it('builds an empty trigger list when every choice is off', () => {
    expect(buildTaskCreationTriggersFromWizardChoices({ branch: '', polling: false, cron: ' ', suggestions: [{ config: pushSuggestion, selected: false }] })).toEqual([]);
    expect(buildTaskCreationTriggersFromWizardChoices({ branch: '', polling: true, cron: '', suggestions: [{ config: pullRequestSuggestion, selected: true }] })).toEqual([
      { type: 'polling', interval_secs: 300, branch: null },
      pullRequestSuggestion,
    ]);
  });

  it('shows the reason of a failed access test and still lets the user continue', () => {
    const fixture = render({}, { testRepositoryCloneAccess: () => throwError(() => new ApiError('Permission denied (publickey)', 400)) });
    const comp = fixture.componentInstance;
    comp.repositoryChanged('ssh://git@forgejo.example.com/acme/app.git');
    comp.continueFromRepository();
    comp.testAccess();
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).querySelector('[role="alert"]')?.textContent).toContain('Permission denied (publickey)');
    comp.continueToTriggers();
    expect(comp.step()).toBe('triggers');
  });

  it('needs no deploy key for HTTPS repositories', () => {
    const getSSHKey = vi.fn(() => of('ssh-ed25519 AAAA'));
    const fixture = render({}, {}, { getSSHKey });
    const comp = fixture.componentInstance;
    comp.repositoryChanged('https://forgejo.example.com/acme/app');
    comp.continueFromRepository();
    fixture.detectChanges();
    expect(getSSHKey).not.toHaveBeenCalled();
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Repositories over HTTPS or git:// need no key.');
  });

  it('refuses a cron schedule with the wrong number of fields', () => {
    const createTask = vi.fn(() => of({ id: 't1', name: 'app', url: '' }));
    const comp = render({}, { createTask }).componentInstance;
    comp.repositoryChanged('https://forgejo.example.com/acme/app');
    comp.continueFromRepository();
    comp.continueToTriggers();
    comp.form.cron = '0 2 *';
    comp.createTaskWithTriggersAndOpenTask();
    expect(comp.cronError).toContain('Enter 5, 6, or 7 cron fields');
    expect(createTask).not.toHaveBeenCalled();
  });

  it('follows later URL edits until the user names the task', () => {
    const comp = render({}, {}).componentInstance;
    comp.repositoryChanged('file:///etc');
    expect(comp.form.display_name).toBe('');
    comp.repositoryChanged('git://gradient.local/states');
    expect(comp.form.name).toBe('states');
    comp.form.display_name = 'My States';
    comp.repositoryChanged('git://gradient.local/other');
    expect(comp.form.display_name).toBe('My States');
    expect(comp.form.name).toBe('states');
  });

  it('refuses an invalid wildcard before the next step', () => {
    const comp = render({}, {}).componentInstance;
    comp.repositoryChanged('ssh://git@forgejo.example.com/acme/my-app.git');
    comp.form.wildcard = 'packages.#';
    comp.continueFromRepository();
    expect(comp.wildcardError).toContain('#');
    expect(comp.step()).toBe('repository');
  });

  it('stays on the last step and shows the reason when creating fails', () => {
    const fixture = render({}, { createTask: () => throwError(() => new ApiError('Task name taken', 409)) });
    const comp = fixture.componentInstance;
    comp.repositoryChanged('https://forgejo.example.com/acme/app');
    comp.continueFromRepository();
    comp.continueToTriggers();
    comp.createTaskWithTriggersAndOpenTask();
    fixture.detectChanges();
    expect(comp.step()).toBe('triggers');
    expect(comp.createError()).toBe('Task name taken');
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Task name taken');
  });

  it('marks the empty display name and name fields on submit', () => {
    const fixture = render({}, {});
    fixture.componentInstance.continueFromRepository();
    fixture.detectChanges();
    const errors = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.field-error')).map((e) => e.textContent?.trim());
    expect(errors).toEqual(['Enter the repository URL.', 'Enter a display name.', 'Enter a name.']);
    expect(fixture.componentInstance.step()).toBe('repository');
  });

  it('warns about a missing cache before the task exists, since creating it starts the first evaluation', () => {
    const el = render({}, {}).nativeElement as HTMLElement;
    expect(el.querySelector('[data-testid="no-cache-banner"]')).not.toBeNull();
  });

  it('shows no cache warning when the project has an active cache', () => {
    const fixture = render({}, {}, { getSubscribedCaches: () => of([{ name: 'main', status: 'active' }]) });
    expect((fixture.nativeElement as HTMLElement).querySelector('[data-testid="no-cache-banner"]')).toBeNull();
  });

  it('keeps the cache warning when the only active cache is read-only', () => {
    const fixture = render({}, {}, { getSubscribedCaches: () => of([{ name: 'main', status: 'active', mode: 'ReadOnly' }]) });
    expect((fixture.nativeElement as HTMLElement).querySelector('[data-testid="no-cache-banner"]')).not.toBeNull();
  });

  it('names a pending subscription request instead of asking for a new cache', () => {
    const fixture = render({}, {}, { getSubscribedCaches: () => of([{ name: 'main', status: 'pending', mode: 'ReadWrite' }]) });
    const banner = (fixture.nativeElement as HTMLElement).querySelector('[data-testid="no-cache-banner"]');
    expect(banner?.textContent).toContain('Subscription request pending');
    expect(banner?.querySelector('a')?.getAttribute('href')).toBe('/project/acme/caches');
  });

  it('shows the no-access state instead of the form to a role that cannot create tasks, without a request', () => {
    const getSubscribedCaches = vi.fn(() => of([]));
    const el = render({}, {}, { getSubscribedCaches }, ['viewProject']).nativeElement as HTMLElement;
    expect(el.querySelector('#connect-repo')).toBeNull();
    expect(el.querySelector('gr-load-state')?.textContent).toContain('Only the members with Write or Admin on this project can view this.');
    expect(getSubscribedCaches).not.toHaveBeenCalled();
  });

  it('tells a visitor of a public project who can connect a repository, without permission names', () => {
    const el = render({}, {}, {}, []).nativeElement as HTMLElement;
    expect(el.querySelector('gr-load-state')?.textContent).toContain('Only the members of this project can view this.');
    expect(el.querySelector('gr-load-state')?.textContent).not.toContain('permission');
  });
});
