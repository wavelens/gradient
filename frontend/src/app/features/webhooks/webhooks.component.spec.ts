/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ApplicationRef } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { EMPTY, Observable, Subject, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { ApiError } from '@core/services/api.service';
import { WebhooksComponent } from './webhooks.component';
import { WebhooksService } from '@core/services/webhooks.service';
import { CachesService } from '@core/services/caches.service';
import { ProjectsService } from '@core/services/projects.service';
import { TeamsService } from '@core/services/teams.service';
import { EventsService } from '@core/services/events.service';
import { NotifyService } from '@core/services/notify.service';
import { AuthService } from '@core/services/auth.service';
import type { CatalogEntry, DeliveryListItem_WebhookDeliveryId, WebhookResponse } from '@core/models';
import { webhooksBase } from '@core/models/webhook.model';
import { EVERY_CACHE_PERMISSION, EVERY_PERMISSION } from '@core/resolvers/access-route.testing';

const hook: WebhookResponse = {
  id: 'w1',
  scope: 'project',
  name: 'ci',
  url: 'https://example.com/hook',
  events: ['build.*'],
  active: true,
  last_fired_at: null,
  created_by: 'u',
  created_at: '2026-09-26T12:00:00',
  updated_at: '2026-09-26T12:00:00',
};
const newestDelivery: DeliveryListItem_WebhookDeliveryId = {
  id: 'd1',
  event: 'build.failed',
  success: true,
  response_status: 200,
  error_message: null,
  duration_ms: 80,
  delivered_at: '2026-09-26T12:05:00',
};

function withLastDelivery(webhook: WebhookResponse, last_delivery: DeliveryListItem_WebhookDeliveryId | null): WebhookResponse {
  return Object.assign({ ...webhook }, { last_delivery });
}

function setup(
  data: Record<string, unknown>,
  params: Record<string, string>,
  list: () => Observable<WebhookResponse[]> = () => of([hook]),
  catalog: CatalogEntry[] = [],
) {
  const service = {
    list: vi.fn(list),
    create: vi.fn(() => of({ webhook: hook, secret: 'whs_abc' })),
    update: vi.fn(() => of(hook)),
    delete: vi.fn(() => of({ deleted: true })),
    test: vi.fn(),
    rotateSecret: vi.fn(() => of({ secret: 'whs_new' })),
    deliveries: vi.fn((): Observable<DeliveryListItem_WebhookDeliveryId[]> => of([])),
    delivery: vi.fn(),
  };
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
  TestBed.configureTestingModule({
    imports: [WebhooksComponent],
    providers: [
      provideRouter([]),
      { provide: WebhooksService, useValue: service },
      { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'Acme' }) } },
      { provide: CachesService, useValue: { getCache: () => of({ display_name: 'Main' }) } },
      { provide: TeamsService, useValue: { get: () => of({ display_name: 'Platform' }) } },
      { provide: EventsService, useValue: { catalog$: of(catalog) } },
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: {
            data: { projectAccess: { project: { permissions: EVERY_PERMISSION } }, teamAccess: { team: { role: 'admin' } }, ...data },
            paramMap: convertToParamMap(params),
          },
          parent: { snapshot: { data: { cacheAccess: { cache: { permissions: EVERY_CACHE_PERMISSION } } } } },
        },
      },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
      { provide: AuthService, useValue: { user: () => ({ superuser: false }) } },
    ],
  });
  const fixture = TestBed.createComponent(WebhooksComponent);
  fixture.detectChanges();
  return { fixture, component: fixture.componentInstance, service, notify, confirm, root: fixture.nativeElement as HTMLElement };
}

function rowButton(root: HTMLElement, label: string): HTMLButtonElement {
  return Array.from(root.querySelectorAll<HTMLButtonElement>('.webhook-actions button')).find(
    (b) => b.querySelector('.gr-button__label')?.textContent === label,
  )!;
}

function rowMenuItem(root: HTMLElement, label: string): HTMLElement {
  root.querySelector<HTMLButtonElement>('.webhook-actions button[aria-label="Actions for ci"]')!.click();
  TestBed.inject(ApplicationRef).tick();
  return Array.from(document.querySelectorAll<HTMLElement>('.gr-menu__item')).find((item) => item.textContent?.includes(label))!;
}

describe('WebhooksComponent', () => {
  it('keeps Test, Deliveries and Edit on the row and the rarer actions in its menu', () => {
    const { root } = setup({ webhookScope: 'project' }, { project: 'p' });
    const labels = Array.from(root.querySelectorAll('.webhook-actions .gr-button__label')).map((label) => label.textContent);
    expect(labels).toEqual(['Test', 'Deliveries', 'Edit']);
    rowMenuItem(root, 'Delete');
    const items = Array.from(document.querySelectorAll('.gr-menu__item')).map((item) => item.textContent?.replace(item.querySelector('.gr-menu__icon')?.textContent ?? '', '').trim());
    expect(items).toEqual(['Rotate secret', 'Disable', 'Delete']);
  });

  it.each([
    { scope: 'project', trail: ['Projects', 'Acme', 'Settings', 'Event notifications'] },
    { scope: 'cache', trail: ['Caches', 'Main', 'Settings', 'Event notifications'] },
    { scope: 'team', trail: ['Teams', 'Platform', 'Settings', 'Event notifications'] },
    { scope: 'instance', trail: ['Admin', 'Event notifications'] },
  ])('leads the $scope scope back to the page its webhooks are reached from', ({ scope, trail }) => {
    const { root } = setup({ webhookScope: scope }, { [scope]: 'main' });
    const crumbs = Array.from(root.querySelectorAll('.breadcrumb-link, .breadcrumb-current'));
    expect(crumbs.map((crumb) => crumb.textContent?.trim())).toEqual(trail);
  });

  it('reads its scope from the route', () => {
    const { component, service } = setup({ webhookScope: 'cache' }, { cache: 'main' });
    expect(component.scope).toEqual({ kind: 'cache', name: 'main' });
    expect(service.list).toHaveBeenCalledWith({ kind: 'cache', name: 'main' });
  });

  it('team scope builds /teams/{team}/webhooks base', () => {
    const { component, service } = setup({ webhookScope: 'team' }, { team: 'platform' });
    expect(component.scope).toEqual({ kind: 'team', name: 'platform' });
    expect(webhooksBase(component.scope)).toBe('teams/platform/webhooks');
    expect(service.list).toHaveBeenCalledWith({ kind: 'team', name: 'platform' });
  });

  it('shows the secret once after create', () => {
    const { component, notify } = setup({ webhookScope: 'project' }, { project: 'p' });
    component.name.set('ci');
    component.url.set('https://example.com/hook');
    component.save();
    expect(component.revealedSecret()).toBe('whs_abc');
    expect(notify.success).toHaveBeenCalledWith('Event notification saved');
    component.revealedSecret.set(null);
    expect(component.webhooks()[0]).not.toHaveProperty('secret');
  });

  it('sends picked events plus typed globs', () => {
    const { component, service } = setup({ webhookScope: 'instance' }, {});
    component.name.set('all');
    component.url.set('https://example.com/hook');
    component.events.set(['task.star']);
    component.globs.set('gc.*, build.*');
    component.save();
    expect(service.create).toHaveBeenCalledWith(
      { kind: 'instance' },
      expect.objectContaining({ events: ['task.star', 'gc.*', 'build.*'] }),
    );
  });

  it('closes an untouched form on Cancel and asks first once something was typed', () => {
    const { fixture, component } = setup({ webhookScope: 'team' }, { team: 'platform' });
    const cancel = () => [...document.querySelectorAll<HTMLButtonElement>('[grDialogFooter] button')].find((b) => b.textContent?.includes('Cancel'))!;
    component.startCreate();
    fixture.detectChanges();
    cancel().click();
    fixture.detectChanges();
    expect(component.formOpen()).toBe(false);

    component.startCreate();
    component.name.set('Deploy hook');
    fixture.detectChanges();
    cancel().click();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog__discard')?.textContent).toContain('Discard changes?');
    expect(component.formOpen()).toBe(true);
  });

  it('keeps subscribed events outside the scope visible in the globs field', () => {
    const { component, service } = setup({ webhookScope: 'team' }, { team: 'platform' });
    component.startEdit({ ...hook, events: ['team.worker.create', 'evaluation.completed', 'build.*'] });
    expect(component.events()).toEqual(['team.worker.create']);
    expect(component.globs()).toBe('evaluation.completed, build.*');
    component.save();
    expect(service.update).toHaveBeenCalledWith(
      { kind: 'team', name: 'platform' },
      'w1',
      expect.objectContaining({ events: ['team.worker.create', 'evaluation.completed', 'build.*'] }),
    );
  });

  it.each([
    { scope: 'project', hint: 'e.g. build.*, evaluation.*' },
    { scope: 'cache', hint: 'e.g. cache.*, team.grant.*' },
    { scope: 'team', hint: 'e.g. team.*' },
    { scope: 'instance', hint: 'e.g. build.*, cache.nar.*' },
  ])('suggests globs the $scope scope can receive', ({ scope, hint }) => {
    const { fixture, component } = setup({ webhookScope: scope }, { [scope]: 'main' });
    component.startCreate();
    fixture.detectChanges();
    const field = document.querySelector('#webhook-globs')!.closest('gr-form-field')!;
    expect(field.textContent).toContain(hint);
  });

  it.each([
    { scope: 'project', groups: ['Build', 'Project', 'Cache'] },
    { scope: 'cache', groups: ['Cache'] },
    { scope: 'team', groups: ['Team'] },
    { scope: 'instance', groups: ['Login', 'Build', 'Project', 'Cache', 'Team', 'GC'] },
  ])('offers the $scope scope only the event groups it can receive', ({ scope, groups }) => {
    const catalog = ['login.success', 'build.failed', 'project.update', 'cache.update', 'cache.subscription.approve', 'team.worker.create', 'gc.swept']
      .map((name) => ({ name, durable: true }));
    const { fixture, component } = setup({ webhookScope: scope }, { [scope]: 'main' }, () => of([hook]), catalog);
    component.startCreate();
    fixture.detectChanges();
    const legends = Array.from(document.querySelectorAll('.gr-dialog legend')).map((legend) => legend.textContent?.trim());
    expect(legends).toEqual(groups);
  });

  it('names the empty name and URL under their fields and sends nothing', () => {
    const { fixture, component, service } = setup({ webhookScope: 'project' }, { project: 'p' });
    component.startCreate();
    fixture.detectChanges();
    const dialog = document.querySelector('.gr-dialog')!;
    Array.from(dialog.querySelectorAll<HTMLButtonElement>('button')).find((b) => b.textContent?.includes('Save'))!.click();
    fixture.detectChanges();
    expect(dialog.querySelector('#webhook-name')!.closest('gr-form-field')!.textContent).toContain('Enter a name.');
    expect(dialog.querySelector('#webhook-url')!.closest('gr-form-field')!.textContent).toContain('Enter the URL to receive events.');
    expect(service.create).not.toHaveBeenCalled();
    expect(component.formOpen()).toBe(true);
  });

  it('refuses a URL without scheme, even on Enter', () => {
    const { fixture, component, service } = setup({ webhookScope: 'project' }, { project: 'p' });
    component.startCreate();
    component.name.set('ci');
    component.url.set('not-a-url');
    fixture.detectChanges();
    const url = document.querySelector<HTMLInputElement>('#webhook-url')!;
    url.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }));
    fixture.detectChanges();
    expect(url.closest('gr-form-field')!.textContent).toContain('https://');
    expect(service.create).not.toHaveBeenCalled();
  });

  it('keeps a refused save inside the open webhook dialog', () => {
    const { fixture, component, service, root } = setup({ webhookScope: 'project' }, { project: 'p' });
    service.create.mockReturnValue(throwError(() => new ApiError('unknown event filter', 422)));
    component.startCreate();
    component.name.set('ci');
    component.url.set('https://example.com/hook');
    component.save();
    fixture.detectChanges();
    expect(component.formOpen()).toBe(true);
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('unknown event filter');
    expect(root.textContent).not.toContain('unknown event filter');

    component.startCreate();
    expect(component.formError()).toBeNull();
  });

  it('shows a refused URL under the URL field until the URL changes', () => {
    const { fixture, component, service } = setup({ webhookScope: 'project' }, { project: 'p' });
    service.create.mockReturnValue(throwError(() => new ApiError("URL host 'localhost' is not allowed", 422)));
    component.startCreate();
    component.name.set('ci');
    component.url.set('http://localhost/hook');
    component.save();
    fixture.detectChanges();
    const field = document.querySelector('#webhook-url')!.closest('gr-form-field')!;
    expect(field.textContent).toContain("URL host 'localhost' is not allowed");
    expect(component.formError()).toBeNull();

    component.url.set('https://example.com/hook');
    fixture.detectChanges();
    expect(field.textContent).not.toContain('localhost');
  });

  it.each([
    { refusal: new ApiError('webhook already exists', 409), input: '#webhook-name' },
    { refusal: new ApiError("invalid event pattern 'Build'", 422), input: '#webhook-globs' },
  ])('shows the refusal "$refusal.message" under its field and focuses it', async ({ refusal, input }) => {
    Element.prototype.scrollIntoView = vi.fn();
    const { fixture, component, service } = setup({ webhookScope: 'project' }, { project: 'p' });
    service.create.mockReturnValue(throwError(() => refusal));
    component.startCreate();
    component.name.set('ci');
    component.url.set('https://example.com/hook');
    component.globs.set('Build');
    fixture.detectChanges();
    component.save();
    fixture.detectChanges();
    await fixture.whenStable();
    const control = document.querySelector<HTMLInputElement>(input)!;
    expect(control.closest('gr-form-field')!.textContent).toContain(refusal.message);
    expect(component.formError()).toBeNull();
    expect(document.activeElement).toBe(control);
  });

  it.each([
    { action: 'toggle', summary: "Couldn't disable event notification", run: (c: WebhooksComponent) => c.toggleActive(hook) },
    { action: 'test', summary: "Couldn't send test event", run: (c: WebhooksComponent) => c.test(hook) },
  ])('reports a failed $action as a toast', ({ summary, run }) => {
    const { component, service, notify } = setup({ webhookScope: 'project' }, { project: 'p' });
    const failure = new ApiError('database is down', 500);
    service.update.mockReturnValue(throwError(() => failure));
    service.test.mockReturnValue(throwError(() => failure));
    run(component);
    expect(notify.failure).toHaveBeenCalledWith(summary, failure);
  });

  it.each([
    { action: 'delete', label: 'Delete', title: 'Delete event notification', method: 'delete' as const },
    { action: 'rotate', label: 'Rotate secret', title: 'Rotate event notification secret', method: 'rotateSecret' as const },
  ])('asks before the $action, naming the webhook', ({ label, title, method }) => {
    const { root, service, confirm } = setup({ webhookScope: 'project' }, { project: 'p' });
    rowMenuItem(root, label).click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title, target: 'ci' }));
    expect(service[method]).toHaveBeenCalledWith({ kind: 'project', name: 'p' }, 'w1');
  });

  it.each([
    { action: 'delete', label: 'Delete', method: 'delete' as const },
    { action: 'rotate', label: 'Rotate secret', method: 'rotateSecret' as const },
  ])('sends no $action when the confirmation is cancelled', ({ label, method }) => {
    const { root, service, confirm, component } = setup({ webhookScope: 'project' }, { project: 'p' });
    confirm.mockImplementation(() => EMPTY);
    rowMenuItem(root, label).click();
    expect(service[method]).not.toHaveBeenCalled();
    expect(component.revealedSecret()).toBeNull();
  });

  it('confirms a deletion with a toast and a reload', () => {
    const { root, service, notify } = setup({ webhookScope: 'project' }, { project: 'p' });
    rowMenuItem(root, 'Delete').click();
    expect(notify.success).toHaveBeenCalledWith('Event notification deleted');
    expect(service.list).toHaveBeenCalledTimes(2);
  });

  it('keeps the webhook form open while saving', () => {
    const { fixture, component, service } = setup({ webhookScope: 'project' }, { project: 'p' });
    service.create.mockReturnValue(new Subject<{ webhook: WebhookResponse; secret: string }>());
    component.startCreate();
    component.name.set('ci');
    component.url.set('https://example.com/hook');
    component.save();
    fixture.detectChanges();
    document.querySelector('.gr-dialog')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    fixture.detectChanges();
    expect(component.formOpen()).toBe(true);
    expect(document.querySelector('.gr-dialog')?.getAttribute('aria-busy')).toBe('true');
  });

  it('closes the one-time secret only through its acknowledge button', () => {
    const { fixture, component } = setup({ webhookScope: 'project' }, { project: 'p' });
    component.rotate(hook);
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog__close')).toBeNull();
    document.querySelector('.gr-dialog')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    document.querySelector<HTMLElement>('.gr-dialog-backdrop')!.click();
    fixture.detectChanges();
    expect(component.revealedSecret()).toBe('whs_new');

    Array.from(document.querySelectorAll<HTMLButtonElement>('.gr-dialog__footer button')).find((b) => b.querySelector('.gr-button__label')?.textContent === "I've copied it")!.click();
    expect(component.revealedSecret()).toBeNull();
  });

  it('sends one test request for two clicks on the row button', () => {
    const { fixture, service, root } = setup({ webhookScope: 'project' }, { project: 'p' });
    service.test.mockReturnValue(new Subject());
    rowButton(root, 'Test').click();
    fixture.detectChanges();
    rowButton(root, 'Test').click();
    expect(service.test).toHaveBeenCalledTimes(1);
  });

  it('locks the row menu while a disable request is open', () => {
    const { fixture, service, root } = setup({ webhookScope: 'project' }, { project: 'p' });
    service.update.mockReturnValue(new Subject());
    rowMenuItem(root, 'Disable').click();
    fixture.detectChanges();
    expect(service.update).toHaveBeenCalledWith({ kind: 'project', name: 'p' }, 'w1', { active: false });
    expect(root.querySelector('.webhook-actions button[aria-label="Actions for ci"]')!.getAttribute('aria-disabled')).toBe('true');
  });

  it.each([
    { hooks: [], inEmptyState: 1 },
    { hooks: [hook], inEmptyState: 0 },
  ])('only offer the create action once, in the empty state while no event notifications exist', ({ hooks, inEmptyState }) => {
    const { root } = setup({ webhookScope: 'team' }, { team: 'research' }, () => of(hooks));
    const create = Array.from(root.querySelectorAll('button')).filter((b) => b.textContent?.includes('New event notification'));
    expect(create).toHaveLength(1);
    expect(root.querySelectorAll('gr-empty-state button')).toHaveLength(inEmptyState);
  });

  it('explains a refused list instead of offering to create webhooks', () => {
    const { root } = setup({ webhookScope: 'cache' }, { cache: 'main' }, () => throwError(() => new ApiError('Forbidden', 403)));
    expect(root.textContent).toContain('No access');
    expect(root.textContent).toContain('No permission');
    expect(root.textContent).not.toContain('No event notifications');
    expect(root.textContent).not.toContain('New event notification');
  });

  it('explains the missing permission to a project viewer without asking the server', () => {
    const { root, service } = setup(
      { webhookScope: 'project', projectAccess: { project: { permissions: ['viewProject'] } } },
      { project: 'p' },
    );
    expect(service.list).not.toHaveBeenCalled();
    expect(root.textContent).toContain('No access');
    expect(root.textContent).toContain('manage event notifications permission for this project');
    expect(root.textContent).not.toContain('New Webhook');
    expect(root.textContent).not.toContain('Try Again');
  });

  it('tells a non-member of the project that only members can view webhooks', () => {
    const { root, service } = setup(
      { webhookScope: 'project', projectAccess: { project: { permissions: [] } } },
      { project: 'p' },
    );
    expect(service.list).not.toHaveBeenCalled();
    expect(root.textContent).toContain('Only the members of this project can view this.');
  });

  it('shows a team member no access without asking the server', () => {
    const { root, service } = setup({ webhookScope: 'team', teamAccess: { team: { role: 'member' } } }, { team: 'platform' });
    expect(service.list).not.toHaveBeenCalled();
    expect(root.textContent).toContain('No access');
    expect(root.textContent).toContain('Only the admins of this team can view this.');
    expect(root.textContent).not.toContain('Try Again');
  });

  it('shows failed deliveries as an error, never as no deliveries', () => {
    const { component, service } = setup({ webhookScope: 'project' }, { project: 'p' });
    service.deliveries.mockReturnValueOnce(throwError(() => new ApiError('database is down', 500)));
    component.openDeliveries(hook);
    TestBed.inject(ApplicationRef).tick();
    expect(service.deliveries).toHaveBeenCalledWith({ kind: 'project', name: 'p' }, 'w1', 50, 0);
    expect(document.body.textContent).toContain('Failed to load deliveries');
    expect(document.querySelector('.gr-dialog')?.textContent).not.toContain('No deliveries yet');

    document.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    TestBed.inject(ApplicationRef).tick();
    expect(service.deliveries).toHaveBeenLastCalledWith({ kind: 'project', name: 'p' }, 'w1', 50, 0);
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('No deliveries yet');
  });

  it('marks an active webhook as failing with the reason of the last delivery in the list, without a request per card', () => {
    const failed = { ...newestDelivery, success: false, response_status: null, error_message: 'send HTTP request: connection refused' };
    const { root, service } = setup({ webhookScope: 'project' }, { project: 'p' }, () => of([withLastDelivery(hook, failed)]));
    expect(service.deliveries).not.toHaveBeenCalled();
    expect(root.querySelector('.webhook-meta gr-badge')?.textContent?.trim()).toBe('Failing');
    expect(root.querySelector('.newest-delivery')?.textContent).toContain('connection refused');
  });

  it('shows a failed test as the newest delivery with its reason', () => {
    const { fixture, root, service, notify } = setup({ webhookScope: 'project' }, { project: 'p' }, () => of([withLastDelivery(hook, newestDelivery)]));
    service.test.mockReturnValue(of({ ...newestDelivery, id: 'd2', success: false, response_status: 404 }));
    rowButton(root, 'Test').click();
    fixture.detectChanges();
    expect(root.querySelector('.webhook-meta gr-badge')?.textContent?.trim()).toBe('Failing');
    expect(root.querySelector('.newest-delivery')?.textContent).toContain('HTTP 404');
    expect(rowButton(root, 'Test').disabled).toBe(false);
    expect(notify.failure).toHaveBeenCalledWith("Couldn't deliver test event: HTTP 404", null);
  });

  it('confirms a delivered test with a toast', () => {
    const { root, service, notify } = setup({ webhookScope: 'project' }, { project: 'p' });
    service.test.mockReturnValue(of(newestDelivery));
    rowButton(root, 'Test').click();
    expect(notify.success).toHaveBeenCalledWith('Test event delivered');
  });

  it.each([
    { state: 'delivered', webhook: withLastDelivery(hook, newestDelivery), chip: 'Active', severity: 'success', line: 'Last delivery' },
    { state: 'never delivered', webhook: withLastDelivery(hook, null), chip: 'Active', severity: 'success', line: 'No deliveries yet' },
    { state: 'inactive', webhook: withLastDelivery({ ...hook, active: false }, { ...newestDelivery, success: false }), chip: 'Inactive', severity: 'warning', line: 'Last delivery' },
  ])('shows the $state webhook as $chip', ({ webhook, chip, severity, line }) => {
    const { root, component } = setup({ webhookScope: 'project' }, { project: 'p' }, () => of([webhook]));
    expect(root.querySelector('.webhook-meta gr-badge')?.textContent?.trim()).toBe(chip);
    expect(component.getWebhookCardStatusChip(webhook).severity).toBe(severity);
    expect(root.querySelector('.webhook-meta')?.textContent).toContain(line);
  });
});
