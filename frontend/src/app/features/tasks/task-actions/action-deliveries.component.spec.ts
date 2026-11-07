/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting, HttpTestingController } from '@angular/common/http/testing';
import { ActionDeliveriesComponent } from './action-deliveries.component';

describe('ActionDeliveriesComponent', () => {
  let fixture: ComponentFixture<ActionDeliveriesComponent>;
  let httpMock: HttpTestingController;

  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [ActionDeliveriesComponent],
      providers: [provideHttpClient(), provideHttpClientTesting()],
    }).compileComponents();
    fixture = TestBed.createComponent(ActionDeliveriesComponent);
    fixture.componentRef.setInput('project', 'test-project');
    fixture.componentRef.setInput('task', 'test-proj');
    fixture.componentRef.setInput('actionId', 'a-id');
    httpMock = TestBed.inject(HttpTestingController);
    fixture.detectChanges();
  });

  afterEach(() => httpMock.verify());

  it('fetches deliveries on init', () => {
    const req = httpMock.expectOne((r) => r.url.includes('/actions/a-id/deliveries'));
    req.flush({ error: false, message: [] });
    expect(fixture.componentInstance.deliveries()).toEqual([]);
  });

  it('lazy-loads detail on expand', () => {
    const delivery = { id: 'd1', event: 'build.failed', success: false, response_status: 500, error_message: 'x', duration_ms: 100, delivered_at: '2026-05-23' };
    httpMock.expectOne((r) => r.url.includes('/actions/a-id/deliveries')).flush({ error: false, message: [delivery] });
    fixture.detectChanges();
    fixture.componentInstance.toggleRow('d1');
    const detailReq = httpMock.expectOne((r) => r.url.includes('/deliveries/d1'));
    detailReq.flush({ error: false, message: { ...delivery, request_body: '{}', response_body: '{"err":1}' } });
    expect(fixture.componentInstance.expanded().get('d1')).toBeTruthy();
  });

  it('shows the delivery time as a relative time with the full date on hover, not the raw timestamp', () => {
    const deliveredAt = new Date(Date.now() - 5 * 60_000).toISOString().replace('Z', '').replace('T', ' ');
    const delivery = { id: 'd1', event: 'test', success: false, response_status: null, error_message: 'x', duration_ms: 100, delivered_at: deliveredAt };
    httpMock.expectOne((r) => r.url.includes('/actions/a-id/deliveries')).flush({ error: false, message: [delivery] });
    fixture.detectChanges();

    const cell = document.querySelector<HTMLTableCellElement>('.gr-dialog tbody td')!;
    expect(cell.textContent?.trim()).toBe('5m ago');
    expect(cell.title).not.toContain(deliveredAt);
    expect(cell.title.length).toBeGreaterThan(0);
  });

  it('shows a failed load as an error with Try Again, never as no deliveries', () => {
    httpMock
      .expectOne((r) => r.url.includes('/actions/a-id/deliveries'))
      .flush({ error: true, message: 'database is down' }, { status: 500, statusText: 'Server Error' });
    fixture.detectChanges();
    expect(document.body.textContent).toContain('Failed to load deliveries');
    expect(document.body.textContent).toContain('database is down');
    expect(document.body.textContent).not.toContain('No deliveries yet');

    document.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    httpMock.expectOne((r) => r.url.includes('/actions/a-id/deliveries')).flush({ error: false, message: [] });
    fixture.detectChanges();
    expect(document.body.textContent).toContain('No deliveries yet');
  });
});
