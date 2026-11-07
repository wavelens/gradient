/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, NEVER, type Observable, of, throwError } from 'rxjs';
import { vi } from 'vitest';
import { TaskActionsComponent } from './task-actions.component';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';
import { ActionsService } from '@core/services/actions.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService } from '@core/services/tasks.service';
import { TeamsService } from '@core/services/teams.service';
import { ConfigService } from '@core/services/config.service';
import { AccessState } from '@core/models/access.model';
import { ActionResponse } from '@core/models';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';

function activatedRouteStub(access: AccessState): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo' }) },
    data: of({}),
    parent: { data: of({ taskAccess: { task: {}, access } }) },
  } as unknown as ActivatedRoute;
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

const action: ActionResponse = {
  id: 'a-1',
  name: 'Notify Ops',
  action_type: 'send_mail',
  config: { type: 'send_mail', recipients: ['ops@example.com'] },
  events: ['build.failed'],
  active: true,
  last_fired_at: null,
  created_by: 'u',
  created_at: '2026-05-01T00:00:00Z',
  updated_at: '2026-05-01T00:00:00Z',
};

function setup(
  access: AccessState,
  actions: ActionResponse[] = [action],
): ComponentFixture<TaskActionsComponent> {
  TestBed.configureTestingModule({
    imports: [TaskActionsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access) },
      {
        provide: ActionsService,
        useValue: {
          list: () => of(actions),
          create: () => of({ action, token: undefined }),
          update: () => of(action),
          delete: () => of({ deleted: true }),
          test: () => of(undefined),
        },
      },
      { provide: IntegrationsService, useValue: { listProjectIntegrations: () => of([]) } },
      { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'Acme' }) } },
      {
        provide: TasksService,
        useValue: {
          getTaskInfo: () => of({ display_name: 'Demo Task' }),
          getTasks: () => of({ items: [], total: 0, page: 1, per_page: 100 }),
        },
      },
      { provide: TeamsService, useValue: { projectGrants: () => of([]) } },
      { provide: ConfigService, useValue: { smtpEnabled: true } },
    ],
  });
  const fixture = TestBed.createComponent(TaskActionsComponent);
  fixture.detectChanges();
  return fixture;
}

describe('TaskActionsComponent', () => {
  it('hides New Action / Edit / Delete / Test buttons under read-only', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'new action')).toBeNull();
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
    expect(findByText(fixture.nativeElement, 'test')).toBeNull();
  });

  it('links to the project integrations', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    const link = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('a')).find((a) => a.textContent?.includes('Integrations'));
    expect(link?.getAttribute('href')).toBe('/project/acme/integrations');
  });

  it('shows but disables write buttons under state-managed access', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const newBtn = findByText(fixture.nativeElement, 'new action') as HTMLButtonElement | null;
    const editBtn = findByText(fixture.nativeElement, 'edit') as HTMLButtonElement | null;
    const deleteBtn = findByText(fixture.nativeElement, 'delete') as HTMLButtonElement | null;
    const testBtn = findByText(fixture.nativeElement, 'test') as HTMLButtonElement | null;
    expect(newBtn).not.toBeNull();
    expect(newBtn!.disabled).toBe(true);
    expect(editBtn).not.toBeNull();
    expect(editBtn!.disabled).toBe(true);
    expect(deleteBtn).not.toBeNull();
    expect(deleteBtn!.disabled).toBe(true);
    expect(testBtn).not.toBeNull();
    // Test is a trigger action, not a config edit - exempt from the managed flag (AccessService.triggerAccess).
    expect(testBtn!.disabled).toBe(false);
  });

  it('leads back through the project, the task and its settings by display name', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const crumbs = Array.from(
      fixture.nativeElement.querySelectorAll('.breadcrumb-link, .breadcrumb-current'),
    ) as HTMLElement[];
    expect(crumbs.map((crumb) => crumb.textContent?.trim())).toEqual(['Projects', 'Acme', 'Demo Task', 'Settings', 'Actions']);
    expect(crumbs[3].getAttribute('href')).toBe('/project/acme/task/demo/settings');
  });

  it('renders action name and event chips', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const text = fixture.nativeElement.textContent ?? '';
    expect(text).toContain('Notify Ops');
    expect(text).toContain('build.failed');
  });

  it('reveals token after create when response includes one', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const comp = fixture.componentInstance;
    (comp as any).actionsService = { create: () => of({ action, token: 'gat_secret' }), list: () => of([action]) };
    comp.onCreateSaved({ name: 'X', config: { type: 'send_web_request', url: 'u' } } as any);
    expect(comp.revealedToken()).toBe('gat_secret');
  });
});

describe('TaskActionsComponent - load errors', () => {
  it('shows a failed load as an error, never as the empty state and loads again on Try Again', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, []);
    const service = TestBed.inject(ActionsService) as unknown as { list: () => unknown };
    service.list = () => throwError(() => new Error('database is down'));
    fixture.componentInstance.loadActions();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load actions');
    expect(root.textContent).toContain('database is down');
    expect(root.textContent).not.toContain('No actions configured');

    service.list = () => of([]);
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(root.textContent).toContain('No actions configured');
  });
});

describe('TaskActionsComponent - write failures', () => {
  const writer = { managed: false, canEdit: true, canTrigger: true };
  const refused = new ApiError('You need write access to change actions', 403);

  it('asks first, naming the action, then deletes it', () => {
    const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
    TestBed.overrideProvider(ConfirmService, { useValue: { confirm } });
    const fixture = setup(writer);
    const remove = vi.fn(() => of({ deleted: true }));
    (TestBed.inject(ActionsService) as unknown as Record<string, unknown>)['delete'] = remove;
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    findByText(fixture.nativeElement, 'delete')!.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete Action', target: 'Notify Ops' }));
    expect(remove).toHaveBeenCalledWith('acme', 'demo', 'a-1');
    expect(success).toHaveBeenCalledWith('Action deleted');
  });

  it('sends nothing when the delete is cancelled', () => {
    TestBed.overrideProvider(ConfirmService, { useValue: { confirm: vi.fn(() => EMPTY) } });
    const fixture = setup(writer);
    const remove = vi.fn(() => of({ deleted: true }));
    (TestBed.inject(ActionsService) as unknown as Record<string, unknown>)['delete'] = remove;
    findByText(fixture.nativeElement, 'delete')!.click();
    expect(remove).not.toHaveBeenCalled();
  });

  it('sends one create when Create is clicked twice', () => {
    const fixture = setup(writer);
    const create = vi.fn(() => NEVER);
    (TestBed.inject(ActionsService) as unknown as Record<string, unknown>)['create'] = create;
    fixture.componentInstance.startCreate();
    fixture.detectChanges();
    const panel = document.querySelector('.gr-dialog') as HTMLElement;
    const fill = (selector: string, value: string) => {
      const input = panel.querySelector<HTMLInputElement>(selector);
      if (!input) return;
      input.value = value;
      input.dispatchEvent(new Event('input'));
    };
    fill('#action-name', 'Notify');
    fill('#action-recipients', 'ops@example.com');
    fill('#action-url', 'https://example.com/hook');
    fixture.detectChanges();
    const submit = Array.from(panel.querySelectorAll('button')).find((b) => b.textContent?.includes('Create'))!;
    submit.click();
    fixture.detectChanges();
    submit.click();
    expect(create).toHaveBeenCalledOnce();
  });

  it('keeps the token on screen until the user confirms the copy', () => {
    const fixture = setup(writer);
    const component = fixture.componentInstance;
    component.revealedToken.set('gat_secret');
    fixture.detectChanges();
    const panel = document.querySelector('.gr-dialog') as HTMLElement;
    panel.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    fixture.detectChanges();
    expect(component.revealedToken()).toBe('gat_secret');
    expect(panel.querySelector('.gr-dialog__close')).toBeNull();
    Array.from(panel.querySelectorAll('button')).find((b) => b.textContent?.includes("I've copied it"))!.click();
    expect(component.revealedToken()).toBeNull();
  });

  it('reports a failed test with the server reason', () => {
    const fixture = setup(writer);
    (TestBed.inject(ActionsService) as unknown as Record<string, unknown>)['test'] = () => throwError(() => refused);
    const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
    fixture.componentInstance.testAction('a-1');
    expect(failure).toHaveBeenCalledWith('Action test failed', refused);
  });

  it('shows a rejected create inside the open dialog and nowhere on the page', () => {
    const fixture = setup(writer);
    (TestBed.inject(ActionsService) as unknown as Record<string, unknown>)['create'] =
      () => throwError(() => new ApiError('An action named X already exists', 409));
    const component = fixture.componentInstance;
    component.startCreate();
    fixture.detectChanges();
    component.onCreateSaved({ name: 'X', config: { type: 'send_web_request', url: 'u' } } as any);
    fixture.detectChanges();

    expect(component.showCreateDialog()).toBe(true);
    const banner = document.body.querySelector('.gr-dialog gr-message-banner');
    expect(banner?.textContent).toContain('An action named X already exists');
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('An action named X already exists');
  });
});
