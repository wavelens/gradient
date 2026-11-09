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
import { TasksService } from '@core/services/tasks.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { ApiError } from '@core/services/api.service';
import { TaskRepositoryComponent } from './task-repository.component';
import { RepositoryAccessComponent } from './repository-access.component';
import { taskAccessData } from '@core/resolvers/access-route.testing';

const state: TaskRepositoryResponse = {
  repository: 'ssh://git@forgejo.example.com/acme/app.git',
  uses_ssh: true,
  deploy_key: 'ssh-ed25519 AAAAkey',
  fetch: { checked_at: '2026-01-01T10:00:00', ok_at: '2026-01-01T10:00:00', failed: false, error: null },
  build_on_push: null,
  build_on_pull_request: null,
  last_event: null,
  report_status: null,
  integrations: [],
  can_edit: true,
  can_manage_integrations: true,
};

function render(getRepository: () => unknown, member = true) {
  const data = taskAccessData({ managed: false, canEdit: member, canTrigger: member });
  if (!member) data.taskAccess.task.permissions = [];
  TestBed.configureTestingModule({
    imports: [TaskRepositoryComponent],
    providers: [
      provideRouter([]),
      {
        provide: ActivatedRoute,
        useValue: { snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'app' }) }, parent: { data: of(data) } },
      },
      { provide: TasksService, useValue: { getRepository, getTaskInfo: () => of({ display_name: 'App' }) } },
      { provide: IntegrationsService, useValue: { listIntegrationDeliveries: () => of([]) } },
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
    expect(el.textContent).toContain('Commit status');
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
});
