/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap } from '@angular/router';
import { of } from 'rxjs';
import { WebhooksComponent } from './webhooks.component';
import { WebhooksService } from '@core/services/webhooks.service';
import { EventsService } from '@core/services/events.service';
import type { Webhook } from '@core/models';

const hook: Webhook = {
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

function setup(data: Record<string, unknown>, params: Record<string, string>) {
  const service = {
    list: vi.fn(() => of([hook])),
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
      { provide: WebhooksService, useValue: service },
      { provide: EventsService, useValue: { catalog$: of([]) } },
      { provide: ActivatedRoute, useValue: { snapshot: { data, paramMap: convertToParamMap(params) } } },
    ],
  });
  const fixture = TestBed.createComponent(WebhooksComponent);
  fixture.detectChanges();
  return { component: fixture.componentInstance, service };
}

describe('WebhooksComponent', () => {
  it('reads its scope from the route', () => {
    const { component, service } = setup({ webhookScope: 'cache' }, { cache: 'main' });
    expect(component.scope).toEqual({ kind: 'cache', name: 'main' });
    expect(service.list).toHaveBeenCalledWith({ kind: 'cache', name: 'main' });
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
});
