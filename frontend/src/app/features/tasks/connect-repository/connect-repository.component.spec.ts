/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { Observable, Subject, of, throwError } from 'rxjs';
import { TasksService } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';
import { TeamsService } from '@core/services/teams.service';
import type { ProjectReadinessResponse } from '@core/models';
import { TriggersService } from '@core/services/triggers.service';
import { ConfigService } from '@core/services/config.service';
import { AuthService } from '@core/services/auth.service';
import { ApiError } from '@core/services/api.service';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import type { GitHostConnectionSummary } from '@core/models';
import { chainWithoutConnection } from '../task-repository/task-git-host-chain.testing';
import { ConnectRepositoryComponent, buildTaskCreationTriggersFromWizardChoices } from './connect-repository.component';

const pushSuggestion = { type: 'reporter_push' as const };
const pullRequestSuggestion = { type: 'reporter_pull_request' as const };
const MISSING_CACHE: ProjectReadinessResponse = {
  ready: false,
  workers: { ready: true, registered: 1, teams: [], teams_granted_without_workers: [], teams_you_can_grant: [] },
  cache: { ready: false, subscriptions: [] },
  repository: { status: 'unchecked', tasks: [] },
};

function render(query: Record<string, string>, tasks: object, projects: object = {}, permissions: string[] = ['createTask', 'editTask'], suggestions: unknown[] = [pushSuggestion, pullRequestSuggestion], connections: GitHostConnectionSummary[] = [], listConnections: () => Observable<GitHostConnectionSummary[]> = () => of(connections), config: object = {}, superuser = false) {
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
      { provide: ProjectsService, useValue: { getProjectReadiness: () => of(MISSING_CACHE), getProject: () => of({ display_name: 'Acme' }), getSSHKey: () => of('ssh-ed25519 AAAA'), ...projects } },
      { provide: TeamsService, useValue: {} },
      { provide: GitHostConnectionsApi, useValue: { list: listConnections } },
      { provide: ConfigService, useValue: { execEnabled: false, ...config } },
      { provide: AuthService, useValue: { user: () => ({ superuser }) } },
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
    const getRepository = vi.fn(() => of({ repository: 'ssh://git@forgejo.example.com/acme/my-app.git', chain: chainWithoutConnection }));
    const fixture = render({}, { createTask, testRepositoryCloneAccess, getRepository });
    const comp = fixture.componentInstance;
    comp.repositoryChanged('ssh://git@forgejo.example.com/acme/my-app.git');
    expect(comp.form.display_name).toBe('my-app');
    expect(comp.form.name).toBe('my-app');
    comp.form.branch = 'release';
    comp.continueFromRepository();
    expect(comp.step()).toBe('git-host');
    comp.continueFromGitHost();
    expect(comp.step()).toBe('access');
    expect(comp.deployKey()).toBe('ssh-ed25519 AAAA');
    comp.testAccess();
    expect(testRepositoryCloneAccess).toHaveBeenCalledWith('acme', 'ssh://git@forgejo.example.com/acme/my-app.git');
    expect(comp.repositoryAccessTestPassedForCurrentUrl()).toBe(true);
    comp.continueToTriggers();
    fixture.detectChanges();
    expect(createTask).not.toHaveBeenCalled();

    const el = fixture.nativeElement as HTMLElement;
    expect(el.textContent).toContain('Build pushes from ssh://git@forgejo.example.com/acme/my-app.git');
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
        { type: 'reporter_push', branches: ['release'] },
        { type: 'time', cron: '0 2 * * *', branch: 'release' },
      ],
    }));
    expect(createTask).toHaveBeenCalledWith('acme', expect.not.objectContaining({ git_host_connection: expect.anything() }));
    expect(getRepository).toHaveBeenCalledWith('acme', 'my-app-1');
    fixture.detectChanges();
    expect(el.querySelector('app-task-git-host-chain')).not.toBeNull();
    expect(el.querySelector('[data-testid="open-task"]')?.getAttribute('href')).toBe('/project/acme/task/my-app-1');
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
    comp.continueFromGitHost();
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
    comp.continueFromGitHost();
    fixture.detectChanges();
    expect(getSSHKey).not.toHaveBeenCalled();
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('Deploy key');
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

  it('shows the project readiness before the task exists, since creating it starts the first evaluation', () => {
    const getProjectReadiness = vi.fn(() => of(MISSING_CACHE));
    const el = render({}, {}, { getProjectReadiness }).nativeElement as HTMLElement;
    expect(getProjectReadiness).toHaveBeenCalledWith('acme');
    expect(el.querySelector('[data-testid="readiness-cache"]')?.textContent).toContain('None writable');
    expect(el.querySelector('[data-testid="no-cache-banner"]')).toBeNull();
  });

  it('shows the no-access state instead of the form to a role that cannot create tasks, without a request', () => {
    const getProjectReadiness = vi.fn(() => of(MISSING_CACHE));
    const el = render({}, {}, { getProjectReadiness }, ['viewProject']).nativeElement as HTMLElement;
    expect(el.querySelector('#connect-repo')).toBeNull();
    expect(el.querySelector('gr-load-state')?.textContent).toContain('Only the members with Write or Admin on this project can view this.');
    expect(getProjectReadiness).not.toHaveBeenCalled();
  });

  it('tells a visitor of a public project who can connect a repository, without permission names', () => {
    const el = render({}, {}, {}, []).nativeElement as HTMLElement;
    expect(el.querySelector('gr-load-state')?.textContent).toContain('Only the members of this project can view this.');
    expect(el.querySelector('gr-load-state')?.textContent).not.toContain('permission');
  });

  it('preselects the only matching connection and sends that selection with the task', () => {
    const fits = { id: 'c1', name: 'forgejo', display_name: 'Forgejo', owner: { type: 'project', name: 'acme' }, git_host_type: 'forgejo',
      base_url: 'https://forgejo.example.com', repository_fit: { fits: true, can_be_selected: true } } as GitHostConnectionSummary;
    const other = { ...fits, id: 'c2', repository_fit: { fits: false, can_be_selected: false, reason: 'Other host' } };
    const createTask = vi.fn(() => of({ id: 't1', name: 'app', url: '' }));
    const fixture = render({}, { createTask, getRepository: () => of({ repository: 'r', chain: chainWithoutConnection }) }, {}, undefined, undefined, [fits, other]);
    const comp = fixture.componentInstance;
    comp.repositoryChanged('https://forgejo.example.com/acme/app');
    comp.continueFromRepository();
    expect(comp.connectionOptions().map((option) => option.value)).toEqual(['', 'c1']);
    expect(comp.selectedConnectionId()).toBe('c1');
    comp.createTaskWithTriggersAndOpenTask();
    expect(createTask).toHaveBeenCalledWith('acme', expect.objectContaining({ git_host_connection: 'c1' }));
  });

  it('lists a connection without host URL marked and never preselects it', () => {
    const withoutHostUrl = { id: 'c3', name: 'forge', display_name: 'Forge', owner: { type: 'project', name: 'acme' }, git_host_type: 'forgejo',
      base_url: null, repository_fit: { fits: false, can_be_selected: true, reason: 'No host URL' } } as GitHostConnectionSummary;
    const fixture = render({}, {}, {}, undefined, undefined, [withoutHostUrl]);
    const comp = fixture.componentInstance;
    comp.repositoryChanged('https://forgejo.example.com/acme/app');
    comp.continueFromRepository();
    expect(comp.connectionOptions().map((option) => [option.value, option.label])).toEqual([
      ['', 'No connection'],
      ['c3', 'Forge · Forgejo · Project acme · No host URL'],
    ]);
    expect(comp.selectedConnectionId()).toBeNull();
  });

  it('discards the connection list that a newer request replaced', () => {
    const requests = [new Subject<GitHostConnectionSummary[]>(), new Subject<GitHostConnectionSummary[]>()];
    const list = vi.fn(() => requests[list.mock.calls.length - 1]);
    const comp = render({}, {}, {}, undefined, undefined, [], list).componentInstance;
    comp.repositoryChanged('https://forgejo.example.com/acme/app');
    comp.continueFromRepository();
    comp.loadConnections();
    expect(requests[0].observed).toBe(false);
    requests[1].next([]);
    expect(comp.connections()).toEqual([]);
  });

  it('shows a failed connection list with a retry instead of an empty list', () => {
    const list = vi.fn(() => throwError(() => new Error('boom')));
    const fixture = render({}, {}, {}, undefined, undefined, [], list);
    fixture.componentInstance.repositoryChanged('https://forgejo.example.com/acme/app');
    fixture.componentInstance.continueFromRepository();
    fixture.detectChanges();
    const el = fixture.nativeElement as HTMLElement;
    expect(el.textContent).toContain('Failed to load git host connections');
    expect(el.querySelector('[data-testid="no-connection"]')).toBeNull();
    el.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    expect(list).toHaveBeenCalledTimes(2);
  });

  it("shows which GitHub accounts don't have the app and offers a new connection for other hosts", () => {
    const github = render({}, {}, {}, ['createTask', 'manageIntegrations'], undefined, [], undefined, { githubAppEnabled: true });
    github.componentInstance.repositoryChanged('https://github.com/acme/app');
    github.componentInstance.continueFromRepository();
    github.detectChanges();
    const githubEl = github.nativeElement as HTMLElement;
    expect(githubEl.querySelector('[data-testid="no-connection"]')?.textContent).toContain('install the GitHub App on acme');
    expect(githubEl.querySelector('[data-testid="new-connection"]')).toBeNull();
    TestBed.resetTestingModule();
    const forgejo = render({}, {}, {}, ['createTask', 'manageIntegrations']);
    forgejo.componentInstance.repositoryChanged('https://forgejo.example.com/acme/app');
    forgejo.componentInstance.continueFromRepository();
    forgejo.detectChanges();
    expect((forgejo.nativeElement as HTMLElement).querySelector('[data-testid="new-connection"]')).not.toBeNull();
  });

  describe('for a GitHub repository with no installation', () => {
    const installUrl = 'https://github.com/apps/gradient-ci/installations/new';

    function gitHostStep(config: object, superuser: boolean, list = vi.fn(() => of<GitHostConnectionSummary[]>([]))) {
      const fixture = render({}, {}, {}, ['createTask', 'manageIntegrations'], undefined, [], list, config, superuser);
      fixture.componentInstance.repositoryChanged('https://github.com/acme/app');
      fixture.componentInstance.continueFromRepository();
      fixture.detectChanges();
      const el = fixture.nativeElement as HTMLElement;
      return { el, list, notice: el.querySelector('[data-testid="no-connection"]')?.textContent ?? '', link: el.querySelector<HTMLAnchorElement>('[data-testid="github-app-install"]') };
    }

    it('shows when the server has no GitHub App', () => {
      const { notice, link } = gitHostStep({ githubAppEnabled: false }, true);
      expect(notice).toContain('No GitHub App on this server');
      expect(link).toBeNull();
    });

    it('lets the instance admin install the app and shows the connection list again after the admin returns', () => {
      const { link, list } = gitHostStep({ githubAppEnabled: true, githubAppInstallUrl: installUrl }, true);
      expect(link?.getAttribute('href')).toBe(installUrl);
      expect(link?.getAttribute('target')).toBe('_blank');
      link!.addEventListener('click', (event) => event.preventDefault());
      link!.click();
      window.dispatchEvent(new Event('focus'));
      expect(list).toHaveBeenCalledTimes(2);
    });

    it('identifies the instance admin as the person who can install the GitHub App for everyone else', () => {
      const { notice, link } = gitHostStep({ githubAppEnabled: true, githubAppInstallUrl: installUrl }, false);
      expect(notice).toContain('An instance admin can install the GitHub App on acme');
      expect(link).toBeNull();
    });
  });
});
