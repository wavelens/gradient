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
import type { WebhookResponse } from '@core/models';
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

function setup(
  data: Record<string, unknown>,
  params: Record<string, string>,
  list: () => Observable<WebhookResponse[]> = () => of([hook]),
) {
  const service = {
    list: vi.fn(list),
    create: vi.fn(() => of({ webhook: hook, secret: 'whs_abc' })),
    update: vi.fn(() => of(hook)),
    delete: vi.fn(() => of({ deleted: true })),
    test: vi.fn(),
    rotateSecret: vi.fn(() => of({ secret: 'whs_new' })),
    deliveries: vi.fn(() => of([])),
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
      { provide: EventsService, useValue: { catalog$: of([]) } },
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: { data: { projectAccess: { project: { permissions: EVERY_PERMISSION } }, ...data }, paramMap: convertToParamMap(params) },
          parent: { snapshot: { data: { cacheAccess: { cache: { permissions: EVERY_CACHE_PERMISSION } } } } },
        },
      },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
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

describe('WebhooksComponent', () => {
  it.each([
    { scope: 'project', trail: ['Projects', 'Acme', 'Settings', 'Event notifications'] },
    { scope: 'cache', trail: ['Caches', 'Main', 'Settings', 'Event notifications'] },
    { scope: 'team', trail: ['Teams', 'Platform', 'Settings', 'Event notifications'] },
    { scope: 'instance', trail: ['Job Board', 'Event notifications'] },
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
    const { component } = setup({ webhookScope: 'project' }, { project: 'p' });
    component.name.set('ci');
    component.url.set('https://example.com/hook');
    component.save();
    expect(component.revealedSecret()).toBe('whs_abc');
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
    service.create.mockReturnValue(throwError(() => new ApiError('Enter a full URL', 422)));
    component.startCreate();
    component.name.set('ci');
    component.url.set('https://example.com/hook');
    component.save();
    fixture.detectChanges();
    expect(component.formOpen()).toBe(true);
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Enter a full URL');
    expect(root.textContent).not.toContain('Enter a full URL');

    component.startCreate();
    expect(component.formError()).toBeNull();
  });

  it.each([
    { action: 'toggle', summary: "Couldn't disable event notification", run: (c: WebhooksComponent) => c.toggleActive(hook) },
    { action: 'test', summary: "Can't send test delivery", run: (c: WebhooksComponent) => c.test(hook) },
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
    rowButton(root, label).click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title, target: 'ci' }));
    expect(service[method]).toHaveBeenCalledWith({ kind: 'project', name: 'p' }, 'w1');
  });

  it.each([
    { action: 'delete', label: 'Delete', method: 'delete' as const },
    { action: 'rotate', label: 'Rotate secret', method: 'rotateSecret' as const },
  ])('sends no $action when the confirmation is cancelled', ({ label, method }) => {
    const { root, service, confirm, component } = setup({ webhookScope: 'project' }, { project: 'p' });
    confirm.mockImplementation(() => EMPTY);
    rowButton(root, label).click();
    expect(service[method]).not.toHaveBeenCalled();
    expect(component.revealedSecret()).toBeNull();
  });

  it('confirms a deletion with a toast and a reload', () => {
    const { root, service, notify } = setup({ webhookScope: 'project' }, { project: 'p' });
    rowButton(root, 'Delete').click();
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

  it.each([
    { action: 'toggle', label: 'Disable', method: 'update' as const },
    { action: 'test', label: 'Test', method: 'test' as const },
  ])('sends one $action request for two clicks on the row button', ({ label, method }) => {
    const { fixture, service, root } = setup({ webhookScope: 'project' }, { project: 'p' });
    service[method].mockReturnValue(new Subject());
    const button = () => Array.from(root.querySelectorAll<HTMLButtonElement>('.webhook-actions button')).find((b) => b.querySelector('.gr-button__label')?.textContent === label)!;
    button().click();
    fixture.detectChanges();
    button().click();
    expect(service[method]).toHaveBeenCalledTimes(1);
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
    expect(root.textContent).toContain('manage webhooks permission for this project');
    expect(root.textContent).not.toContain('New Webhook');
    expect(root.textContent).not.toContain('Try Again');
  });

  it('shows failed deliveries as an error, never as no deliveries', () => {
    const { component, service } = setup({ webhookScope: 'project' }, { project: 'p' });
    service.deliveries.mockReturnValueOnce(throwError(() => new ApiError('database is down', 500)));
    component.openDeliveries(hook);
    TestBed.inject(ApplicationRef).tick();
    expect(document.body.textContent).toContain('Failed to load deliveries');
    expect(document.body.textContent).not.toContain('No deliveries yet');

    document.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    TestBed.inject(ApplicationRef).tick();
    expect(service.deliveries).toHaveBeenCalledTimes(2);
    expect(document.body.textContent).toContain('No deliveries yet');
  });
});
