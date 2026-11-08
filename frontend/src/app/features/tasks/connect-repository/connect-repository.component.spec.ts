/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import { TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { ProjectsService } from '@core/services/projects.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { ApiError } from '@core/services/api.service';
import { ConnectRepositoryComponent } from './connect-repository.component';

const repo: TaskRepositoryResponse = {
  repository: 'ssh://git@forgejo.example.com/acme/app.git',
  uses_ssh: true,
  deploy_key: 'ssh-ed25519 AAAA',
  fetch: null,
  build_on_push: null,
  build_on_pull_request: null,
  last_event: null,
  report_status: null,
  integrations: [],
  can_edit: true,
  can_manage_integrations: true,
};

function render(query: Record<string, string>, tasks: object, projects: object = {}) {
  TestBed.configureTestingModule({
    imports: [ConnectRepositoryComponent],
    providers: [
      provideRouter([]),
      {
        provide: ActivatedRoute,
        useValue: { snapshot: { paramMap: convertToParamMap({ project: 'acme' }), queryParamMap: convertToParamMap(query) } },
      },
      { provide: TasksService, useValue: { checkTaskNameAvailable: () => of(true), getRepository: () => of(repo), ...tasks } },
      { provide: ProjectsService, useValue: { getSubscribedCaches: () => of([]), getProject: () => of({ display_name: 'Acme' }), ...projects } },
      { provide: IntegrationsService, useValue: { listIntegrationDeliveries: () => of([]) } },
    ],
  });
  const fixture = TestBed.createComponent(ConnectRepositoryComponent);
  fixture.detectChanges();
  return fixture;
}

describe('ConnectRepositoryComponent', () => {
  it('derives the task name from the repository URL and creates the task', () => {
    const createTask = vi.fn(() => of('id'));
    const fixture = render({}, { createTask });
    const comp = fixture.componentInstance;
    comp.repositoryChanged('ssh://git@forgejo.example.com/acme/my-app.git');
    expect(comp.form.display_name).toBe('my-app');
    expect(comp.form.name).toBe('my-app');
    comp.createTask();
    expect(createTask).toHaveBeenCalledWith('acme', expect.objectContaining({
      name: 'my-app',
      repository: 'ssh://git@forgejo.example.com/acme/my-app.git',
    }));
    expect(comp.step()).toBe('access');
  });

  it('keeps the form and shows the reason when creating fails', () => {
    const fixture = render({}, { createTask: () => throwError(() => new ApiError('Task name taken', 409)) });
    const comp = fixture.componentInstance;
    comp.repositoryChanged('https://forgejo.example.com/acme/app');
    comp.createTask();
    expect(comp.step()).toBe('repository');
    expect(comp.createError()).toBe('Task name taken');
  });

  it('resumes a half-done setup at the step from the link', () => {
    const fixture = render({ task: 'app', step: 'events' }, {});
    expect(fixture.componentInstance.step()).toBe('events');
    expect(fixture.componentInstance.state()).toEqual(repo);
  });

  it('warns about a missing cache before the task exists, since creating it starts the first evaluation', () => {
    const el = render({}, {}).nativeElement as HTMLElement;
    expect(el.querySelector('[data-testid="cache-missing"]')).not.toBeNull();
  });

  it('shows no cache warning when the project has an active cache', () => {
    const fixture = render({}, {}, { getSubscribedCaches: () => of([{ name: 'main', status: 'active' }]) });
    expect((fixture.nativeElement as HTMLElement).querySelector('[data-testid="cache-missing"]')).toBeNull();
  });

  it('ends on the events step with a link to the task', () => {
    const fixture = render({ task: 'app', step: 'events' }, {});
    const link = (fixture.nativeElement as HTMLElement).querySelector('a[href="/project/acme/task/app"]');
    expect(link?.textContent).toContain('Open task');
  });
});
