/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { By } from '@angular/platform-browser';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import { TaskRepositoryResponse } from '@core/models';
import { chainWithoutConnection } from './task-git-host-chain.testing';
import { TasksService } from '@core/services/tasks.service';
import { ApiError } from '@core/services/api.service';
import { TaskRepositoryComponent } from './task-repository.component';
import { RepositoryAccessComponent } from './repository-access.component';
import { taskAccessData } from '@core/resolvers/access-route.testing';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';

const state: TaskRepositoryResponse = {
  repository: 'ssh://git@forgejo.example.com/acme/app.git',
  uses_ssh: true,
  deploy_key: 'ssh-ed25519 AAAAkey',
  fetch: { checked_at: '2026-01-01T10:00:00', ok_at: '2026-01-01T10:00:00', failed: false, error: null },
  recent_deliveries: [],
  chain: chainWithoutConnection,
  report_status: false,
  status_posts: [],
  available_connections: [],
  can_edit: true,
  can_manage_integrations: true,
};

function render(getRepository: () => unknown, member = true, canEdit = member, tasks: object = {}) {
  const data = taskAccessData({ managed: false, canEdit, canTrigger: canEdit });
  if (!member) data.taskAccess.task.permissions = [];
  TestBed.configureTestingModule({
    imports: [TaskRepositoryComponent],
    providers: [
      provideRouter([]),
      {
        provide: ActivatedRoute,
        useValue: { snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'app' }) }, parent: { data: of(data) } },
      },
      { provide: TasksService, useValue: { getRepository, getTaskInfo: () => of({ display_name: 'App' }), ...tasks } },
      { provide: GitHostConnectionsApi, useValue: {} },
    ],
  });
  const fixture = TestBed.createComponent(TaskRepositoryComponent);
  fixture.detectChanges();
  return fixture;
}

describe('TaskRepositoryComponent', () => {
  it('shows access, deploy key and the event settings of the task in one page', () => {
    const getRepository = vi.fn(() => of(state));
    const el = render(getRepository).nativeElement as HTMLElement;
    expect(getRepository).toHaveBeenCalledWith('acme', 'app');
    expect(el.querySelector('[data-testid="fetch-ok"]')).not.toBeNull();
    expect(Array.from(el.querySelectorAll('textarea')).map((t) => t.value)).toContain('ssh-ed25519 AAAAkey');
    expect(el.textContent).toContain('Git host connection');
  });

  it('shows a load error with a retry instead of an empty page', () => {
    const el = render(() => throwError(() => new ApiError('boom', 500))).nativeElement as HTMLElement;
    expect(el.textContent).toContain('Failed to load repository');
  });

  it('shows a visitor outside the project no access and asks the server nothing', () => {
    const getRepository = vi.fn(() => of(state));
    const el = render(getRepository, false).nativeElement as HTMLElement;
    expect(getRepository).not.toHaveBeenCalled();
    expect(el.textContent).toContain('Only the members of this project can view this.');
    expect(el.textContent).not.toContain('Try Again');
  });

  it('tells a view-only member once in the title chip that the page is read-only', () => {
    const el = render(() => of({ ...state, can_edit: false, can_manage_integrations: false }), true, false).nativeElement as HTMLElement;
    expect(el.querySelectorAll('gr-message-banner').length).toBe(0);
    expect(el.querySelector('app-settings-access-state-badge')?.textContent).toContain('Only the admins of this project can change these settings.');
    expect(el.querySelector('[data-testid="read-only"]')).toBeNull();
  });

  it('asks before leaving with an unsaved repository URL', () => {
    const fixture = render(() => of(state));
    const access = fixture.debugElement.query(By.directive(RepositoryAccessComponent)).componentInstance as RepositoryAccessComponent;
    expect(fixture.componentInstance.canDeactivate()).toBe(true);
    access.startEdit();
    expect(fixture.componentInstance.canDeactivate()).toBe(true);
    access.draftUrl.set('ssh://git@forgejo.example.com/acme/other.git');
    expect(fixture.componentInstance.canDeactivate()).toBeInstanceOf(Observable);
    access.cancelEdit();
    expect(fixture.componentInstance.canDeactivate()).toBe(true);
  });

  it('shows the chain first and runs its actions on the sections below', () => {
    Element.prototype.scrollIntoView = vi.fn();
    const getRepository = vi.fn(() => of(state));
    const installWebhook = vi.fn(() => of('ok'));
    const fixture = render(getRepository, true, true, { installWebhook });
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('gr-settings-section app-task-git-host-chain')).not.toBeNull();
    const access = fixture.debugElement.query(By.directive(RepositoryAccessComponent)).componentInstance as RepositoryAccessComponent;
    fixture.componentInstance.runChainAction({ type: 'edit_repository_url' });
    expect(access.editing()).toBe(true);
    fixture.componentInstance.runChainAction({ type: 'install_webhook' });
    expect(installWebhook).toHaveBeenCalledWith('acme', 'app');
    expect(getRepository).toHaveBeenCalledTimes(2);
  });
});
