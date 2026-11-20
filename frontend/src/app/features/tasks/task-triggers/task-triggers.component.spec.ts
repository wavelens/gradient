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
import { ProjectsService } from '@core/services/projects.service';
import { TasksService } from '@core/services/tasks.service';
import { chainWithoutConnection } from '../task-repository/task-git-host-chain.testing';

const repositoryWithChain = {
  repository: 'https://forgejo.example.com/acme/app',
  chain: { ...chainWithoutConnection, connection: { ...chainWithoutConnection.connection, action: { type: 'select_connection' as const } } },
};
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
  config: { interval_secs: 300, branch: null },
  last_fired_at: null,
  created_at: '2026-01-01T00:00:00',
};

const list = vi.fn();
const getSuggestedPushAndPullRequestTriggerConfigs = vi.fn();
let suggested: unknown[] = [];

function setup(
  access: AccessState,
  triggers: any[] = [trigger],
  update: (...args: any[]) => unknown = () => of({}),
  query: Record<string, string> = {},
  task: Partial<TaskDetailsResponse> = {},
): ComponentFixture<TaskTriggersComponent> {
  list.mockReset().mockReturnValue(of(triggers));
  getSuggestedPushAndPullRequestTriggerConfigs.mockReset().mockReturnValue(of(suggested));
  TestBed.configureTestingModule({
    imports: [TaskTriggersComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access, query, task) },
      { provide: TriggersService, useValue: { list, update, getSuggestedPushAndPullRequestTriggerConfigs } },
      { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'Acme' }) } },
      { provide: TasksService, useValue: { getTaskInfo: () => of({ display_name: 'Demo Task' }), getRepository: () => of(repositoryWithChain) } },
    ],
  });
  const fixture = TestBed.createComponent(TaskTriggersComponent);
  fixture.detectChanges();
  return fixture;
}

describe('TaskTriggersComponent - access gating', () => {
  it('hides New Trigger / Edit / Delete / Poll now buttons under read-only', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'new trigger')).toBeNull();
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
    expect(findByText(fixture.nativeElement, 'poll now')).toBeNull();
  });

  it('shows a visitor outside the project no access, asks the server nothing and offers no Repository page', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false }, [trigger], undefined, {}, { permissions: [] });
    const el = fixture.nativeElement as HTMLElement;
    expect(list).not.toHaveBeenCalled();
    expect(el.textContent).toContain('Only the members of this project can view this.');
    expect(el.textContent).not.toContain('Try Again');
    expect(Array.from(el.querySelectorAll('a')).some((a) => a.textContent?.includes('Repository'))).toBe(false);
  });

  it('offers all five trigger kinds when creating a trigger', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    fixture.componentInstance.startCreate();
    fixture.detectChanges();
    expect(fixture.componentInstance.typeOptions.map((o) => o.value)).toEqual(['polling', 'reporter_push', 'reporter_pull_request', 'time', 'post_request']);
    expect(document.querySelector('#trig-type')).not.toBeNull();
  });

  it('summarises a push trigger by its filters', () => {
    const reporterTrigger = {
      ...trigger,
      id: 't2',
      type: 'reporter_push' as const,
      config: { type: 'reporter_push', branches: ['main'], tags: ['v*'] },
    };
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [reporterTrigger]);
    expect(fixture.nativeElement.textContent ?? '').toContain('branches: main / tags: v*');
  });

  it('shows the git host chain in a push trigger and leads its actions to the Repository page', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, []);
    fixture.componentInstance.startCreate();
    fixture.componentInstance.form.type = 'reporter_push';
    fixture.detectChanges();
    const chain = document.querySelector('[data-testid="trigger-connection-chain"]');
    expect(chain?.textContent).toContain('No connection to forgejo.example.com');
    expect(chain?.querySelector('[data-testid="chain-action-select_connection"]')?.getAttribute('href')).toBe('/project/acme/task/demo/repository');
  });

  it('opens the new trigger dialog with the type from the link', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [trigger], undefined, { new_trigger: 'reporter_push' });
    expect(fixture.componentInstance.showCreateDialog()).toBe(true);
    expect(fixture.componentInstance.form.type).toBe('reporter_push');
  });

  it('ignores a new trigger link without edit access or with an unknown type', () => {
    expect(setup({ managed: false, canEdit: false, canTrigger: false }, [trigger], undefined, { new_trigger: 'reporter_push' })
      .componentInstance.showCreateDialog()).toBe(false);
    TestBed.resetTestingModule();
    expect(setup({ managed: false, canEdit: true, canTrigger: true }, [trigger], undefined, { new_trigger: 'unknown' })
      .componentInstance.showCreateDialog()).toBe(false);
  });

  it('hides write buttons under state-managed access', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    expect(fixture.nativeElement.querySelector('app-settings-access-state-badge')?.textContent).toContain('server configuration');
    const fireBtn = findByText(fixture.nativeElement, 'poll now') as HTMLButtonElement | null;
    expect(findByText(fixture.nativeElement, 'new trigger')).toBeNull();
    expect(findByText(fixture.nativeElement, 'edit')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
    expect(fireBtn).not.toBeNull();
    // Poll now can start an evaluation. The button doesn't edit config, so the managed flag can't disable it (AccessService.triggerAccess).
    expect(fireBtn!.disabled).toBe(false);
  });
});

describe('TaskTriggersComponent - default trigger', () => {
  it('hides Delete and Make Default on the default trigger', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [{ ...trigger, is_default: true }]);
    expect(findByText(fixture.nativeElement, 'delete')).toBeNull();
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
    config: { type: 'reporter_push', include_flake_inputs: true },
    last_fired_at: null,
    };

  it('keeps include_flake_inputs when an edited push trigger is saved', () => {
    const update = vi.fn((..._args: any[]) => of({}));
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [pushTrigger], update);
    const component = fixture.componentInstance;
    component.startEdit(pushTrigger as any);
    component.form.branches = 'main';
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    component.saveEdit();
    expect(update.mock.calls[0][3].config.include_flake_inputs).toBe(true);
    expect(success).toHaveBeenCalledWith('Trigger saved');
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
    expect((fixture.nativeElement as HTMLElement).textContent).toMatch(/Last fired: just now/);
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
    const create = vi.fn((..._args: any[]) => of({}));
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

  it('creates a push trigger without more fields', () => {
    const { component, create } = withCreate();
    component.startCreate();
    component.form.type = 'reporter_push';
    component.saveCreate();
    expect(create).toHaveBeenCalledWith('acme', 'demo', { config: { type: 'reporter_push' }, active: true });
  });

  it('refuses a cron expression with the wrong number of fields', () => {
    const { fixture, component, create } = withCreate();
    component.startCreate();
    component.form.type = 'time';
    component.form.cron = '0 2 * *';
    component.saveCreate();
    fixture.detectChanges();
    expect(fieldError()).toContain('Enter 5, 6, or 7 fields');
    expect(create).not.toHaveBeenCalled();

    component.form.cron = '0 2 * * *';
    component.form.branch = ' release ';
    component.saveCreate();
    expect(create).toHaveBeenCalledWith('acme', 'demo', { config: { type: 'time', cron: '0 2 * * *', branch: 'release' }, active: true });
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
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    document.querySelector('#trig-branch')!.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter' }));
    expect(create).toHaveBeenCalledOnce();
    expect(success).toHaveBeenCalledWith('Trigger created');
  });

  it.each(['polling', 'reporter_push', 'reporter_pull_request', 'post_request'] as const)('sends both path filter lists for a %s trigger', (type) => {
    const { component, create } = withCreate();
    component.startCreate();
    component.form.type = type;
    component.form.include_paths = ' src/*, flake.nix ';
    component.form.exclude_paths = 'docs/*';
    component.saveCreate();
    expect(create).toHaveBeenCalledWith('acme', 'demo', expect.objectContaining({
      config: expect.objectContaining({ type, include_paths: ['src/*', 'flake.nix'], exclude_paths: ['docs/*'] }),
    }));
  });

  it.each(['polling', 'reporter_push', 'reporter_pull_request', 'time', 'post_request'] as const)('leaves empty path filter lists out of a %s trigger', (type) => {
    const { component, create } = withCreate();
    component.startCreate();
    component.form.type = type;
    component.form.cron = '0 2 * * *';
    component.form.include_paths = ' , ';
    component.saveCreate();
    const config = create.mock.calls[0][2].config;
    expect(config).not.toHaveProperty('include_paths');
    expect(config).not.toHaveProperty('exclude_paths');
  });

  it('loads the path filters into the edit form and drops a cleared list', () => {
    const filtered = { ...trigger, config: { ...trigger.config, include_paths: ['src/*'], exclude_paths: ['docs/*', '*.md'] } };
    const { component, update } = withCreate();
    component.startEdit(filtered as any);
    expect(component.form.exclude_paths).toBe('docs/*, *.md');
    component.form.include_paths = '';
    component.saveEdit();
    const config = update.mock.calls[0][3].config;
    expect(config).not.toHaveProperty('include_paths');
    expect(config.exclude_paths).toEqual(['docs/*', '*.md']);
  });

  it('names the path filters in the summary row', () => {
    const push = { ...trigger, id: 't2', type: 'reporter_push' as const, config: { type: 'reporter_push', branches: ['main'], include_paths: ['src/*'], exclude_paths: ['docs/*'] } };
    const polling = { ...trigger, config: { ...trigger.config, include_paths: ['nix/*'] } };
    const postRequest = { ...trigger, id: 't3', type: 'post_request' as const, config: { type: 'post_request', branch: 'main', exclude_paths: ['*.md'] } };
    const fixture = setup(writer, [polling, push, postRequest]);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('branches: main / include paths: src/* / exclude paths: docs/*');
    expect(text).toContain('every 5 min, include paths: nix/*');
    expect(text).toContain('branch main, exclude paths: *.md');
  });

  it('closes the edit dialog without a request when nothing changed', () => {
    const { component, update } = withCreate();
    component.startEdit(trigger as any);
    component.saveEdit();
    expect(update).not.toHaveBeenCalled();
    expect(component.showEditDialog()).toBe(false);
  });
});

describe('TaskTriggersComponent - post request trigger', () => {
  const writer = { managed: false, canEdit: true, canTrigger: true };
  const url = 'https://ci.example.com/api/v1/hooks/trigger/t5';
  const postRequest = {
    ...trigger,
    id: 't5',
    type: 'post_request' as const,
    config: { type: 'post_request', branch: null },
    has_secret: true,
    post_request_url: url,
  };
  const dialogCopyFieldValues = () =>
    Array.from(document.querySelectorAll<HTMLInputElement>('.gr-dialog gr-copy-field input')).map((input) => input.value);

  function renderWithReplace(triggers: unknown[], confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action())) {
    TestBed.overrideProvider(ConfirmService, { useValue: { confirm } });
    const fixture = setup(writer, triggers);
    const replace = vi.fn(() => of({ ...postRequest, generated_secret: 'trsec-456' }));
    Object.assign(TestBed.inject(TriggersService), { replacePostRequestTriggerSecret: replace });
    const row = (fixture.nativeElement as HTMLElement).querySelector('.trigger-row') as HTMLElement;
    return { fixture, row, replace, confirm };
  }

  it('creates a post request trigger and shows the generated secret once', () => {
    const fixture = setup(writer, []);
    const create = vi.fn(() => of({ ...postRequest, generated_secret: 'trsec-123' }));
    Object.assign(TestBed.inject(TriggersService), { create });
    const component = fixture.componentInstance;
    component.startCreate();
    component.form.type = 'post_request';
    component.form.branch = ' main ';
    component.saveCreate();
    fixture.detectChanges();

    expect(create).toHaveBeenCalledWith('acme', 'demo', { config: { type: 'post_request', branch: 'main' }, active: true });
    expect(component.createdTriggerWithOneTimeSecret()).toEqual({ url, secret: 'trsec-123' });
    expect(dialogCopyFieldValues()).toEqual([url, 'trsec-123']);
    (document.querySelector('[data-testid="secret-done"]') as HTMLButtonElement).click();
    expect(component.createdTriggerWithOneTimeSecret()).toBeNull();
  });

  it('replaces the secret of a post request trigger and shows the new one', () => {
    const { fixture, row, replace, confirm } = renderWithReplace([postRequest]);
    expect(row.querySelector<HTMLInputElement>('gr-copy-field input')?.value).toBe(url);
    expect(findByText(row, 'generate secret')).toBeNull();

    findByText(row, 'replace secret')!.click();
    fixture.detectChanges();

    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Replace Secret' }));
    expect(replace).toHaveBeenCalledWith('acme', 'demo', 't5');
    expect(dialogCopyFieldValues()).toEqual([url, 'trsec-456']);
  });

  it('gives a post request trigger without a secret one without asking first', () => {
    const { fixture, row, replace, confirm } = renderWithReplace([{ ...postRequest, has_secret: false }]);
    expect(row.textContent).toContain('No secret');
    expect(findByText(row, 'replace secret')).toBeNull();

    findByText(row, 'generate secret')!.click();
    fixture.detectChanges();

    expect(confirm).not.toHaveBeenCalled();
    expect(replace).toHaveBeenCalledWith('acme', 'demo', 't5');
    expect(fixture.componentInstance.createdTriggerWithOneTimeSecret()?.secret).toBe('trsec-456');
  });

  it('double-click "Generate secret" on a trigger without a secret to send one secret request', () => {
    const { fixture, row, replace } = renderWithReplace([{ ...postRequest, has_secret: false }]);
    replace.mockReturnValue(NEVER);

    findByText(row, 'generate secret')!.click();
    fixture.detectChanges();
    findByText(row, 'generate secret')!.click();

    expect(replace).toHaveBeenCalledTimes(1);
  });

  it('hides Replace secret on a state-managed task', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true }, [postRequest]);
    expect(findByText(fixture.nativeElement, 'replace secret')).toBeNull();
  });
});

describe('TaskTriggersComponent - push triggers next to polling', () => {
  const writer = { managed: false, canEdit: true, canTrigger: true };
  const push = {
    ...trigger,
    id: 't-push',
    type: 'reporter_push' as const,
    config: { type: 'reporter_push', branches: [] },
  };
  const repository = { repository: 'ssh://git@forgejo.example.com/acme/app.git' };
  afterEach(() => (suggested = []));

  it('offers suggested push and pull request triggers unticked and creates one on Add', () => {
    const pr = { type: 'reporter_pull_request', actions: ['opened'] };
    suggested = [push.config, pr];
    const fixture = setup(writer, [trigger], undefined, {}, repository);
    const create = vi.fn(() => of({}));
    Object.assign(TestBed.inject(TriggersService), { create });

    const section = (fixture.nativeElement as HTMLElement).querySelector('[data-testid="trigger-suggestions"]')!;
    expect(getSuggestedPushAndPullRequestTriggerConfigs).toHaveBeenCalledWith('acme', repository.repository);
    expect(section.textContent).toContain('Pull request');
    expect(create).not.toHaveBeenCalled();
    findByText(section as HTMLElement, 'add')!.click();
    expect(create).toHaveBeenCalledWith('acme', 'demo', { config: push.config, active: true });
  });

  it('hides suggestions the task already has', () => {
    suggested = [push.config];
    const fixture = setup(writer, [trigger, push], undefined, {}, repository);
    expect(getSuggestedPushAndPullRequestTriggerConfigs).toHaveBeenCalled();
    expect((fixture.nativeElement as HTMLElement).querySelector('[data-testid="trigger-suggestions"]')).toBeNull();
  });

  it('slows frequent polling to hourly in one click once the push trigger has fired', () => {
    const update = vi.fn((..._args: any[]) => of({}));
    const fixture = setup(writer, [trigger, { ...push, last_fired_at: new Date().toISOString() }], update);
    const banner = (fixture.nativeElement as HTMLElement).querySelector('[data-testid="polling-fallback"]') as HTMLElement;
    expect(banner.textContent).toContain('An hourly poll can catch pushes');
    findByText(banner, 'poll every hour')!.click();
    expect(update).toHaveBeenCalledWith('acme', 'demo', 't1', { config: expect.objectContaining({ type: 'polling', interval_secs: 3600 }) });
  });

  it('shows no polling hint before the push trigger fired, without an active push trigger, or when polling is hourly', () => {
    const fired = { ...push, last_fired_at: new Date().toISOString() };
    const hourly = { ...trigger, config: { ...trigger.config, interval_secs: 3600 } } as typeof trigger;
    for (const triggers of [[trigger, push], [trigger, { ...fired, active: false }], [hourly, fired]]) {
      const fixture = setup(writer, triggers);
      expect((fixture.nativeElement as HTMLElement).querySelector('[data-testid="polling-fallback"]')).toBeNull();
      fixture.destroy();
      TestBed.resetTestingModule();
    }
  });

  it('names whole hours without a zero minute part', () => {
    const fixture = setup(writer, [{ ...trigger, config: { ...trigger.config, interval_secs: 3600 } } as typeof trigger]);
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('every 1h');
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('0 min');
  });

  it('labels polling rows with Poll now and the next poll', () => {
    const fixture = setup(writer, [{ ...trigger, last_fired_at: new Date(Date.now() - 60_000).toISOString() }]);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(findByText(fixture.nativeElement, 'poll now')).not.toBeNull();
    expect(text).toContain('Next poll in 4m');
  });
});
