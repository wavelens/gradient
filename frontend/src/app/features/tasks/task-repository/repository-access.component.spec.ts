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
import { ApiError } from '@core/services/api.service';
import { ConfirmService } from '@shared/confirm';
import { RepositoryAccessComponent } from './repository-access.component';

const state: TaskRepositoryResponse = {
  repository: 'git@forgejo.example.com:acme/app.git',
  uses_ssh: true,
  deploy_key: 'ssh-ed25519 AAAAkey',
  fetch: null,
  last_event: null,
  report_status: false,
  status_posts: [],
  available_connections: [],
  can_edit: true,
  can_manage_integrations: true,
};

function render(overrides: Partial<TaskRepositoryResponse>, tasks: object) {
  TestBed.configureTestingModule({
    imports: [RepositoryAccessComponent],
    providers: [provideRouter([]), { provide: TasksService, useValue: tasks }],
  });
  const fixture = TestBed.createComponent(RepositoryAccessComponent);
  fixture.componentRef.setInput('project', 'acme');
  fixture.componentRef.setInput('task', 'app');
  fixture.componentRef.setInput('state', { ...state, ...overrides });
  fixture.componentRef.setInput('gitHost', 'forgejo');
  fixture.detectChanges();
  return fixture;
}

describe('RepositoryAccessComponent', () => {
  it('links to the deploy key settings of the repository on the Git host', () => {
    const el = render({}, {}).nativeElement as HTMLElement;
    const hrefs = Array.from(el.querySelectorAll('a')).map((a) => a.getAttribute('href'));
    expect(hrefs).toContain('https://forgejo.example.com/acme/app/settings/keys');
  });

  it('tests access to the saved URL and reloads the state either way', () => {
    const checkRepository = vi.fn(() => throwError(() => new ApiError('Task not found', 404)));
    const fixture = render({}, { checkRepository });
    const changed = vi.fn();
    fixture.componentInstance.changed.subscribe(changed);
    fixture.componentInstance.testAccess();
    expect(checkRepository).toHaveBeenCalledWith('acme', 'app', 'git@forgejo.example.com:acme/app.git');
    expect(fixture.componentInstance.testError()).toContain('Task not found');
    expect(changed).toHaveBeenCalled();
  });

  it('leaves a failed fetch of the saved URL to the banner instead of repeating it', () => {
    const checkRepository = vi.fn(() => throwError(() => new ApiError('Permission denied (publickey)', 400, 'repository_unreachable')));
    const fixture = render({}, { checkRepository });
    fixture.componentInstance.testAccess();
    expect(fixture.componentInstance.testError()).toBeNull();
  });

  it('checks a changed URL before saving it and records the check afterwards', () => {
    const updateTask = vi.fn(() => of('ok'));
    const checkRepository = vi.fn(() => of('1a2b'));
    const fixture = render({}, { updateTask, checkRepository });
    const comp = fixture.componentInstance;
    const changed = vi.fn();
    comp.changed.subscribe(changed);
    comp.startEdit();
    comp.draftUrl.set('git://gradient.local/states');
    comp.saveUrl();
    expect(updateTask).toHaveBeenCalledWith('acme', 'app', { repository: 'git://gradient.local/states' });
    expect(checkRepository).toHaveBeenCalledTimes(2);
    expect(checkRepository).toHaveBeenCalledWith('acme', 'app', 'git://gradient.local/states');
    expect(changed).toHaveBeenCalledTimes(2);
    expect(comp.editing()).toBe(false);
  });

  it('skips the check before saving when the draft URL already passed the test', () => {
    const updateTask = vi.fn(() => of('ok'));
    const checkRepository = vi.fn(() => of('1a2b'));
    const comp = render({}, { updateTask, checkRepository }).componentInstance;
    comp.startEdit();
    comp.draftUrl.set('git://gradient.local/states');
    comp.testAccess();
    comp.saveUrl();
    expect(updateTask).toHaveBeenCalledTimes(1);
    expect(checkRepository).toHaveBeenCalledTimes(2);
  });

  it('asks before saving a URL that Gradient cannot fetch', () => {
    const updateTask = vi.fn(() => of('ok'));
    const checkRepository = vi.fn(() =>
      throwError(() => new ApiError('SSH to git.example.com:2222: failed to fill whole buffer', 400, 'repository_unreachable')));
    const comp = render({}, { updateTask, checkRepository }).componentInstance;
    comp.startEdit();
    comp.draftUrl.set('ssh://git@git.example.com:2222/acme/app.git');
    comp.saveUrl();
    expect(updateTask).not.toHaveBeenCalled();
    expect(comp.editing()).toBe(true);
    const pending = TestBed.inject(ConfirmService).pending();
    expect(pending?.request.target).toBe('ssh://git@git.example.com:2222/acme/app.git');
    expect(pending?.request.consequence).toContain('failed to fill whole buffer');
    expect(pending?.request.consequence).toContain('hostname and port');

    pending?.request.action().subscribe();
    expect(updateTask).toHaveBeenCalledWith('acme', 'app', { repository: 'ssh://git@git.example.com:2222/acme/app.git' });
    expect(comp.editing()).toBe(false);
  });

  it('tests the unsaved URL while editing without touching the saved state', () => {
    const checkRepository = vi.fn(() => of('1a2b'));
    const fixture = render({}, { checkRepository });
    const component = fixture.componentInstance;
    const changed = vi.fn();
    component.changed.subscribe(changed);
    component.startEdit();
    component.draftUrl.set('not a url');
    component.testAccess();
    expect(checkRepository).not.toHaveBeenCalled();
    component.draftUrl.set('git://gradient.local/states');
    component.testAccess();
    expect(checkRepository).toHaveBeenCalledWith('acme', 'app', 'git://gradient.local/states');
    expect(component.unsavedRepositoryUrlAccessTestPassed()).toBe(true);
    expect(changed).not.toHaveBeenCalled();
  });

  it('drops the result of testing a discarded URL on Cancel', () => {
    const checkRepository = vi.fn(() => throwError(() => new ApiError('failed to lookup address information', 400)));
    const fixture = render({ fetch: { failed: false, checked_at: new Date().toISOString() } }, { checkRepository });
    const el = fixture.nativeElement as HTMLElement;
    fixture.componentInstance.startEdit();
    fixture.componentInstance.draftUrl.set('git://nope.invalid/x');
    fixture.componentInstance.testAccess();
    fixture.detectChanges();
    expect(el.querySelector('[role=alert]')?.textContent).toContain('failed to lookup');

    (Array.from(el.querySelectorAll('button')).find((b) => b.textContent?.includes('Cancel')) as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(el.querySelector('[role=alert]')).toBeNull();
    expect(el.querySelector('[data-testid=fetch-ok]')).not.toBeNull();
  });

  it('refuses a local path as repository URL without saving', () => {
    const updateTask = vi.fn(() => of('ok'));
    const fixture = render({}, { updateTask });
    const comp = fixture.componentInstance;
    comp.startEdit();
    comp.draftUrl.set('file:///etc');
    comp.saveUrl();
    expect(comp.urlError()).toContain('remote repository');
    expect(updateTask).not.toHaveBeenCalled();
  });

  it('explains that GitHub takes a deploy key on one repository only', () => {
    const el = render({ repository: 'git@github.com:acme/app.git' }, {}).nativeElement as HTMLElement;
    expect(el.querySelector('[data-testid="github-deploy-key"]')?.textContent).toContain('only one repository');
  });

  it('hides the edit and test buttons from read-only members', () => {
    const el = render({ can_edit: false }, {}).nativeElement as HTMLElement;
    expect(el.textContent).not.toContain('Test access');
    expect(el.textContent).not.toContain('Edit URL');
  });

  it('installs the deploy key through the Git host and tests access afterwards', () => {
    const connection: GitHostConnectionSummary = {
      id: 'c1',
      owner: { type: 'project', name: 'acme' },
      name: 'forgejo',
      display_name: 'forgejo.example.com',
      git_host_type: 'forgejo',
      receives_events: true,
      reports_status: true,
      can_manage: true,
    };
    const installDeployKey = vi.fn(() => of('acme/app'));
    const checkRepository = vi.fn(() => of('1a2b'));
    const fixture = render({ git_host_connection: connection }, { installDeployKey, checkRepository });
    const el = fixture.nativeElement as HTMLElement;
    (el.querySelector('[data-testid="install-deploy-key"]') as HTMLButtonElement).click();
    expect(installDeployKey).toHaveBeenCalledWith('acme', 'app');
    expect(checkRepository).toHaveBeenCalled();
  });

  it('hides the deploy key install without a git host connection that reports status', () => {
    const el = render({}, {}).nativeElement as HTMLElement;
    expect(el.querySelector('[data-testid="install-deploy-key"]')).toBeNull();
  });
});
