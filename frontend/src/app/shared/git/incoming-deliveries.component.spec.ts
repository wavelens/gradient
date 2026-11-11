/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { IncomingWebhookDeliveryResponse } from '@core/models';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { IncomingDeliveriesComponent, deliverySummary } from './incoming-deliveries.component';

const base: IncomingWebhookDeliveryResponse = {
  id: 'd1',
  event: 'push',
  status_code: 200,
  result: 'queued',
  detail: null,
  repository: 'https://forgejo.example.com/acme/app',
  tasks_scanned: 1,
  queued: 1,
  created_at: '2026-01-01T10:00:00',
};

describe('deliverySummary', () => {
  it.each([
    [base, 'Queued 1 evaluation'],
    [{ ...base, result: 'no_matching_task' as const, queued: 0 }, 'No task tracks https://forgejo.example.com/acme/app'],
    [{ ...base, result: 'refused' as const, status_code: 401, detail: 'invalid webhook signature' }, 'Refused with HTTP 401: invalid webhook signature'],
    [{ ...base, result: 'ignored' as const, event: 'ping', repository: null }, 'Received ping event'],
  ])('summarises a delivery', (delivery, expected) => {
    expect(deliverySummary(delivery)).toBe(expected);
  });
});

describe('IncomingDeliveriesComponent', () => {
  it('lists the deliveries of the git host connection', () => {
    const list = vi.fn(() => of([base, { ...base, id: 'd2', result: 'refused' as const, status_code: 401, detail: 'bad' }]));
    TestBed.configureTestingModule({
      imports: [IncomingDeliveriesComponent],
      providers: [{ provide: GitHostConnectionsApi, useValue: { listDeliveries: list } }],
    });
    const fixture = TestBed.createComponent(IncomingDeliveriesComponent);
    fixture.componentRef.setInput('gitHostConnectionOwner', { type: 'project', name: 'acme' });
    fixture.componentRef.setInput('connectionId', 'i1');
    fixture.detectChanges();
    expect(list).toHaveBeenCalledWith({ type: 'project', name: 'acme' }, 'i1');
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Queued 1 evaluation');
    expect(text).toContain('Refused with HTTP 401: bad');
  });

  it('says in one line without a heading that nothing was delivered yet', () => {
    TestBed.configureTestingModule({
      imports: [IncomingDeliveriesComponent],
      providers: [{ provide: GitHostConnectionsApi, useValue: { listDeliveries: () => of([]) } }],
    });
    const fixture = TestBed.createComponent(IncomingDeliveriesComponent);
    fixture.componentRef.setInput('gitHostConnectionOwner', { type: 'project', name: 'acme' });
    fixture.componentRef.setInput('connectionId', 'i1');
    fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement;
    expect(element.textContent).toContain('No deliveries yet.');
    expect(element.textContent).not.toContain('Recent deliveries');
    expect(element.querySelector('gr-empty-state')).toBeNull();
  });
});
