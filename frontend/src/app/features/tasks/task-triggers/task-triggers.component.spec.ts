/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { taskAccessData } from '@core/resolvers/access-route.testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, NEVER, type Observable, of, throwError } from 'rxjs';
import { vi } from 'vitest';
import { TaskTriggersComponent } from './task-triggers.component';
import { TriggersService } from '@core/services/triggers.service';
import { IntegrationsService } from '@core/services/integrations.service';
import { ProjectsService } from '@core/services/projects.service';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';
import { AccessState } from '@core/models/access.model';
import type { TaskDetailsResponse } from '@core/models';
import { ConfirmService, type ConfirmRequest } from '@shared/confirm';

function activatedRouteStub(access: AccessState, query: Record<string, string> = {}, task: Partial<TaskDetailsResponse> = {}): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ project: 'acme', task: 'demo' }), queryParamMap: convertToParamMap(query) },
    data: of({}),
    parent: { data: of(taskAccessData(access, task)) },
  } as unknown as ActivatedRoute;
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

const trigger = {
  id: 't1',
  type: 'polling' as const,
  active: true,
  config: { type: 'polling', interval_secs: 300, branch: null },
  last_fired_at: null,
  created_at: '2026-01-01T00:00:00',
  integration: null,
};

const list = vi.fn();
const listProjectIntegrationSummaries = vi.fn();

function setup(
  access: AccessState,
  triggers: any[] = [trigger],
  update: (...args: any[]) => unknown = () => of({}),
  query: Record<string, string> = {},
  integrations: unknown[] = [],
  task: Partial<TaskDetailsResponse> = {},
): ComponentFixture<TaskTriggersComponent> {
  list.mockReset().mockReturnValue(of(triggers));
  listProjectIntegrationSummaries.mockReset().mockReturnValue(of(integrations));
  TestBed.configureTestingModule({
    imports: [TaskTriggersComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access, query, task) },
      { provide: TriggersService, useValue: { list, update } },
      { provide: IntegrationsService, useValue: { listProjectIntegrationSummaries } },
      { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'Acme' }) } },
      { provide: TasksService, useValue: { getTaskInfo: () => of({ display_name: 'Demo Task' }) } },
    ],
  });
  const fixture = TestBed.createComponent(TaskTriggersComponent);
  fixture.detectChanges();
  return fixture;
}

describe('TaskTriggersComponent - access gating', () => {
  it('hides New Trigger / Edit / Delete / Fire Now buttons under read-only', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'new trigger')).toBeNull();
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
    expect(findByText(fixture.nativeElement, 'fire now')).toBeNull();
  });

  it('links editors to the Repository page for push and pull request builds, not read-only users', () => {
    const repositoryLink = (fixture: ReturnType<typeof setup>) =>
      Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('a')).find((a) => a.textContent?.includes('Repository'));
    expect(repositoryLink(setup({ managed: false, canEdit: true, canTrigger: true }))?.getAttribute('href')).toMatch(/\/project\/acme\/task\/[^/]+\/repository$/);
    TestBed.resetTestingModule();
    expect(repositoryLink(setup({ managed: false, canEdit: false, canTrigger: false }))).toBeUndefined();
  });

  it('shows a visitor outside the project no access, asks the server nothing and offers no Repository page', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false }, [trigger], undefined, {}, [], { permissions: [] });
    const el = fixture.nativeElement as HTMLElement;
    expect(list).not.toHaveBeenCalled();
    expect(listProjectIntegrationSummaries).not.toHaveBeenCalled();
    expect(el.textContent).toContain('Only the members of this project can view this.');
    expect(el.textContent).not.toContain('Try Again');
    expect(Array.from(el.querySelectorAll('a')).some((a) => a.textContent?.includes('Repository'))).toBe(false);
  });

  it('offers only polling and cron when creating a trigger', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    expect(fixture.componentInstance.createTypeOptions.map((o) => o.value)).toEqual(['polling', 'time']);
  });

  it('renders reporter trigger using inlined integration display_name (not raw UUID)', () => {
    const reporterTrigger = {
      id: 't2',
      type: 'reporter_push' as const,
      active: true,
      config: {
        type: 'reporter_push',
        integration_id: '019e16b2-e958-7652-ad97-67cd7b0fea61',
        branches: ['main'],
      },
      last_fired_at: null,
      integration: {
        id: '019e16b2-e958-7652-ad97-67cd7b0fea61',
        name: 'github',
        display_name: 'GitHub',
        git_host_type: 'github',
      },
    };
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [reporterTrigger]);
    const text = fixture.nativeElement.textContent ?? '';
    expect(text).toContain('from GitHub');
    expect(text).not.toContain('019e16b2-e958-7652-ad97-67cd7b0fea61');
  });

  it('names the Git host of each integration in the picker instead of its raw type', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, []);
    fixture.componentInstance.inboundIntegrations.set([
      { id: 'i1', name: 'forge', display_name: 'Our Forge', kind: 'inbound', git_host_type: 'forgejo' },
    ] as never);
    expect(fixture.componentInstance.integrationOptions().map((o) => o.label)).toContain('Our Forge (Forgejo)');
  });

  it('falls back gracefully when reporter trigger references a deleted integration', () => {
    const orphan = {
      id: 't3',
      type: 'reporter_push' as const,
      active: true,
      config: {
        type: 'reporter_push',
        integration_id: '019e16b2-e958-7652-ad97-67cd7b0fea61',
        branches: ['main'],
      },
      last_fired_at: null,
      integration: null,
    };
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [orphan]);
    const text = fixture.nativeElement.textContent ?? '';
    expect(text).toContain('from deleted integration');
    expect(text).not.toContain('019e16b2-e958-7652-ad97-67cd7b0fea61');
  });

  it('shows but disables write buttons under state-managed access', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    expect(fixture.nativeElement.querySelector('gr-message-banner')?.textContent).toContain('server configuration');
    const newBtn = findByText(fixture.nativeElement, 'new trigger') as HTMLButtonElement | null;
    const editBtn = findByText(fixture.nativeElement, 'edit') as HTMLButtonElement | null;
    const deleteBtn = findByText(fixture.nativeElement, 'delete') as HTMLButtonElement | null;
    const fireBtn = findByText(fixture.nativeElement, 'fire now') as HTMLButtonElement | null;
    expect(newBtn).not.toBeNull();
    expect(newBtn!.disabled).toBe(true);
    expect(editBtn).not.toBeNull();
    expect(editBtn!.disabled).toBe(true);
    expect(deleteBtn).not.toBeNull();
    expect(deleteBtn!.disabled).toBe(true);
    expect(fireBtn).not.toBeNull();
    // Fire Now is a trigger action, not a config edit - exempt from the managed flag (AccessService.triggerAccess).
    expect(fireBtn!.disabled).toBe(false);
  });
});

describe('TaskTriggersComponent - default trigger', () => {
  it('disables Delete and hides Make Default on the default trigger', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [{ ...trigger, is_default: true }]);
    const deleteBtn = findByText(fixture.nativeElement, 'delete') as HTMLButtonElement | null;
    expect(deleteBtn!.disabled).toBe(true);
    expect(findByText(fixture.nativeElement, 'make default')).toBeNull();
  });

  it('marks only inactive triggers with a badge', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [trigger, { ...trigger, id: 't9', active: false }]);
    const badges = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.trigger-meta gr-badge')).map((b) => b.textContent?.trim());
    expect(badges).toEqual(['Inactive']);
  });

  it('lists triggers oldest first, so Make Default never moves a row', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [
      { ...trigger, id: 't3', created_at: '2026-03-01T00:00:00', is_default: true },
      { ...trigger, id: 't2' },
      { ...trigger, id: 't1' },
    ]);
    expect(fixture.componentInstance.triggers().map((t) => t.id)).toEqual(['t1', 't2', 't3']);
  });

  it('offers Make Default on every other trigger', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [{ ...trigger, is_default: false }]);
    const deleteBtn = findByText(fixture.nativeElement, 'delete') as HTMLButtonElement | null;
    expect(deleteBtn!.disabled).toBe(false);
    expect(findByText(fixture.nativeElement, 'make default')).not.toBeNull();
  });
});

describe('TaskTriggersComponent - flake inputs', () => {
  const pushTrigger = {
    id: 't4',
    type: 'reporter_push' as const,
    active: true,
    config: { type: 'reporter_push', integration_id: 'i1', include_flake_inputs: true },
    last_fired_at: null,
    integration: null,
  };

  it('keeps include_flake_inputs when an edited push trigger is saved', () => {
    const update = vi.fn((..._args: any[]) => of({}));
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [pushTrigger], update);
    const component = fixture.componentInstance;
    component.startEdit(pushTrigger as any);
    component.form.branches = 'main';
    component.saveEdit();
    expect(update.mock.calls[0][3].config.include_flake_inputs).toBe(true);
  });

  it('keeps the edit dialog open on Escape while the save runs', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [pushTrigger], () => NEVER);
    const component = fixture.componentInstance;
    component.startEdit(pushTrigger as any);
    component.form.branches = 'main';
    fixture.detectChanges();
    component.saveEdit();
    fixture.detectChanges();
    const panel = document.querySelector('.gr-dialog') as HTMLElement;
    panel.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    fixture.detectChanges();
    expect(component.showEditDialog()).toBe(true);
    expect(panel.getAttribute('aria-busy')).toBe('true');
  });

  it('leaves include_flake_inputs out once the box is cleared', () => {
    const update = vi.fn((..._args: any[]) => of({}));
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [pushTrigger], update);
    const component = fixture.componentInstance;
    component.startEdit(pushTrigger as any);
    component.form.include_flake_inputs = false;
    component.saveEdit();
    expect(update.mock.calls[0][3].config.include_flake_inputs).toBeUndefined();
  });
});

describe('TaskTriggersComponent - load errors', () => {
  it('shows a failed load as an error, never as the empty state and loads again on Try Again', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, []);
    const service = TestBed.inject(TriggersService) as unknown as { list: () => unknown };
    service.list = () => throwError(() => new Error('database is down'));
    fixture.componentInstance.loadTriggers();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load triggers');
    expect(root.textContent).toContain('database is down');
    expect(root.textContent).not.toContain('No triggers configured');

    service.list = () => of([]);
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(root.textContent).toContain('No triggers configured');
  });
});

describe('TaskTriggersComponent - write failures', () => {
  const writer = { managed: false, canEdit: true, canTrigger: true };
  const refused = new ApiError('You need write access to change triggers', 403);

  function failingService(fixture: ComponentFixture<TaskTriggersComponent>): Record<string, unknown> {
    fixture.detectChanges();
    return TestBed.inject(TriggersService) as unknown as Record<string, unknown>;
  }

  it('reports a failed fire with the server reason and frees the row', () => {
    const fixture = setup(writer);
    failingService(fixture)['fireNow'] = () => throwError(() => refused);
    const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
    fixture.componentInstance.fireNow('t1');
    expect(failure).toHaveBeenCalledWith('Failed to fire trigger', refused);
    expect(fixture.componentInstance.firingId()).toBeNull();
  });

  it('shows the new last fired time after Fire Now', () => {
    const fixture = setup(writer);
    failingService(fixture)['fireNow'] = () => of('ok');
    list.mockReturnValue(of([{ ...trigger, last_fired_at: new Date(Date.now() - 1000).toISOString().replace('Z', '') }]));
    fixture.componentInstance.fireNow('t1');
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).textContent).toMatch(/Last fired: \d+s ago/);
  });

  it('reports a failed make default', () => {
    const fixture = setup(writer);
    failingService(fixture)['setDefault'] = () => throwError(() => refused);
    const failure = vi.spyOn(TestBed.inject(NotifyService), 'failure');
    fixture.componentInstance.makeDefault('t1');
    expect(failure).toHaveBeenCalledWith("Couldn't make trigger the default", refused);
    expect(fixture.componentInstance.makingDefaultId()).toBeNull();
  });

  it('asks first, naming the trigger, then deletes it', () => {
    const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
    TestBed.overrideProvider(ConfirmService, { useValue: { confirm } });
    const fixture = setup(writer);
    const remove = vi.fn(() => of(undefined));
    failingService(fixture)['delete'] = remove;
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    findByText(fixture.nativeElement, 'delete')!.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete Trigger', target: 'Polling: every 5 min' }));
    expect(remove).toHaveBeenCalledWith('acme', 'demo', 't1');
    expect(success).toHaveBeenCalledWith('Trigger deleted');
  });

  it('sends nothing when the delete is cancelled', () => {
    TestBed.overrideProvider(ConfirmService, { useValue: { confirm: vi.fn(() => EMPTY) } });
    const fixture = setup(writer);
    const remove = vi.fn(() => of(undefined));
    failingService(fixture)['delete'] = remove;
    findByText(fixture.nativeElement, 'delete')!.click();
    expect(remove).not.toHaveBeenCalled();
  });

  it('shows a rejected save inside the open dialog, not raw transport text', () => {
    const fixture = setup(writer, [trigger], () => throwError(() => new Error('Http failure response for /triggers/t1: 500')));
    const component = fixture.componentInstance;
    component.startEdit(trigger as any);
    component.form.branch = 'main';
    component.saveEdit();
    fixture.detectChanges();
    expect(component.showEditDialog()).toBe(true);
    expect(component.error()).toBe('Failed to update trigger.');
    const banner = document.body.querySelector('.gr-dialog gr-message-banner');
    expect(banner?.textContent).toContain('Failed to update trigger.');

    component.cancelEdit();
    component.startEdit(trigger as any);
    expect(component.error()).toBeNull();
  });
});

describe('TaskTriggersComponent - validation', () => {
  const writer = { managed: false, canEdit: true, canTrigger: true };
  const fieldError = () => document.querySelector('.gr-dialog .field-error')?.textContent?.trim() ?? '';

  function withCreate(update = vi.fn((..._args: any[]) => of({}))) {
    const fixture = setup(writer, [trigger], update);
    const create = vi.fn(() => of({}));
    Object.assign(TestBed.inject(TriggersService), { create });
    return { fixture, component: fixture.componentInstance, create, update };
  }

  it.each([5, null, 12.5])('refuses a polling interval of %s without sending it', (interval) => {
    const { fixture, component, create } = withCreate();
    component.startCreate();
    component.form.interval_secs = interval;
    component.saveCreate();
    fixture.detectChanges();
    expect(fieldError()).toContain('at least 10 seconds');
    expect(create).not.toHaveBeenCalled();
  });

  it('asks for an integration before creating a push trigger', () => {
    const { fixture, component, create } = withCreate();
    component.startCreate();
    component.form.type = 'reporter_push';
    component.saveCreate();
    fixture.detectChanges();
    expect(fieldError()).toBe('Select an integration.');
    expect(create).not.toHaveBeenCalled();
  });

  it('refuses a cron expression with the wrong number of fields', () => {
    const { fixture, component, create } = withCreate();
    component.startCreate();
    component.form.type = 'time';
    component.form.cron = '0 2 * * *';
    component.saveCreate();
    fixture.detectChanges();
    expect(fieldError()).toContain('6 or 7 fields');
    expect(create).not.toHaveBeenCalled();

    component.form.cron = '0 0 2 * * *';
    component.saveCreate();
    expect(create).toHaveBeenCalledWith('acme', 'demo', { config: { type: 'time', cron: '0 0 2 * * *' }, active: true });
  });

  it('describes the schedule in plain words under the cron field and in the list', () => {
    const cronTrigger = { ...trigger, id: 't2', type: 'time' as const, config: { type: 'time', cron: '0 0 9 * * Mon-Fri' } };
    const fixture = setup(writer, [cronTrigger]);
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Every week on Monday to Friday at 09:00 UTC (0 0 9 * * Mon-Fri)');

    const component = fixture.componentInstance;
    component.startCreate();
    component.form.type = 'time';
    component.form.cron = '0 0 2 * * *';
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Every day at 02:00 UTC');
  });

  it('creates on Enter in a single-line field', () => {
    const { fixture, component, create } = withCreate();
    component.startCreate();
    fixture.detectChanges();
    document.querySelector('#trig-branch')!.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter' }));
    expect(create).toHaveBeenCalledOnce();
  });

  it('closes the edit dialog without a request when nothing changed', () => {
    const { component, update } = withCreate();
    component.startEdit(trigger as any);
    component.saveEdit();
    expect(update).not.toHaveBeenCalled();
    expect(component.showEditDialog()).toBe(false);
  });
});

describe('TaskTriggersComponent - setting up an integration from a trigger', () => {
  const writer = { managed: false, canEdit: true, canTrigger: true };
  const orphan = {
    id: 't3',
    type: 'reporter_push' as const,
    active: true,
    config: { type: 'reporter_push', integration_id: 'gone', branches: [] },
    last_fired_at: null,
    integration: null,
  };
  const inbound = { id: 'new-id', name: 'forgejo', display_name: 'Forgejo', kind: 'inbound', git_host_type: 'forgejo' };

  it('links from the edit dialog to Integrations with the way back to this trigger', () => {
    const fixture = setup(writer, [orphan]);
    fixture.componentInstance.startEdit(orphan as any);
    fixture.detectChanges();
    const link = Array.from(document.querySelectorAll<HTMLAnchorElement>('.gr-dialog a')).find((a) => a.textContent?.includes('Set up an integration'));
    expect(link?.getAttribute('href')).toBe('/project/acme/integrations?task=demo&trigger=t3');
  });

  it('reopens the trigger with the new integration selected on the way back', () => {
    const fixture = setup(writer, [orphan], undefined, { edit: 't3', integration: 'new-id' }, [inbound]);
    const component = fixture.componentInstance;
    expect(component.showEditDialog()).toBe(true);
    expect(component.editingTrigger()?.id).toBe('t3');
    expect(component.form.integration_id).toBe('new-id');
  });

  it.each([
    ['read-only', { managed: false, canEdit: false, canTrigger: false }],
    ['state-managed', { managed: true, canEdit: true, canTrigger: true }],
  ])('opens no edit dialog on the way back under %s access', (_label, access) => {
    const fixture = setup(access, [orphan], undefined, { edit: 't3', integration: 'new-id' }, [inbound]);
    expect(fixture.componentInstance.showEditDialog()).toBe(false);
  });
});
