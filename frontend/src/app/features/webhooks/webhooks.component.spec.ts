/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ApplicationRef } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { WebhooksComponent } from './webhooks.component';
import { WebhooksService } from '@core/services/webhooks.service';
import { CachesService } from '@core/services/caches.service';
import { ProjectsService } from '@core/services/projects.service';
import { TeamsService } from '@core/services/teams.service';
import { EventsService } from '@core/services/events.service';
import type { WebhookResponse } from '@core/models';
import { webhooksBase } from '@core/models/webhook.model';

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
  TestBed.configureTestingModule({
    imports: [WebhooksComponent],
    providers: [
      provideRouter([]),
      { provide: WebhooksService, useValue: service },
      { provide: ProjectsService, useValue: { getProject: () => of({ display_name: 'Acme' }) } },
      { provide: CachesService, useValue: { getCache: () => of({ display_name: 'Main' }) } },
      { provide: TeamsService, useValue: { get: () => of({ display_name: 'Platform' }) } },
      { provide: EventsService, useValue: { catalog$: of([]) } },
      { provide: ActivatedRoute, useValue: { snapshot: { data, paramMap: convertToParamMap(params) } } },
    ],
  });
  const fixture = TestBed.createComponent(WebhooksComponent);
  fixture.detectChanges();
  return { component: fixture.componentInstance, service, root: fixture.nativeElement as HTMLElement };
}

describe('WebhooksComponent', () => {
  it.each([
    { scope: 'project', trail: ['Projects', 'Acme', 'Settings', 'Webhooks'] },
    { scope: 'cache', trail: ['Caches', 'Main', 'Settings', 'Webhooks'] },
    { scope: 'team', trail: ['Teams', 'Platform', 'Settings', 'Webhooks'] },
    { scope: 'instance', trail: ['Job Board', 'Webhooks'] },
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

  it('explains a refused list instead of offering to create webhooks', () => {
    const { root } = setup({ webhookScope: 'cache' }, { cache: 'main' }, () => throwError(() => new ApiError('Forbidden', 403)));
    expect(root.textContent).toContain('Failed to load webhooks');
    expect(root.textContent).toContain('No permission');
    expect(root.textContent).not.toContain('No webhooks');
    expect(root.textContent).not.toContain('New Webhook');
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
