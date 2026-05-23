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
    fixture.componentRef.setInput('org', 'test-org');
    fixture.componentRef.setInput('project', 'test-proj');
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
});
