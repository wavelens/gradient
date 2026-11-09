/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Observable, Subject, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import {
  OutgoingWebhookDeliveryListDialog,
  RecordedOutgoingDelivery,
  RecordedOutgoingDeliveryListRow,
  getDeliveryResultReason,
} from './outgoing-webhook-delivery-list-dialog.component';

const failed: RecordedOutgoingDeliveryListRow = {
  id: 'd1',
  event: 'build.failed',
  success: false,
  response_status: null,
  error_message: 'send HTTP request: client error (Connect): connection refused',
  duration_ms: 120,
  delivered_at: '2026-05-23T10:00:00',
};
const delivered: RecordedOutgoingDeliveryListRow = { ...failed, id: 'd2', success: true, response_status: 200, error_message: null };

function render(
  page: (limit: number, offset: number) => Observable<RecordedOutgoingDeliveryListRow[]>,
  detail: (id: string) => Observable<RecordedOutgoingDelivery> = (id) => of({ ...failed, id, request_body: '{"event":"build.failed"}', response_body: null }),
) {
  const loadDeliveryPage = vi.fn(page);
  const loadDeliveryDetail = vi.fn(detail);
  TestBed.configureTestingModule({ imports: [OutgoingWebhookDeliveryListDialog] });
  const fixture: ComponentFixture<OutgoingWebhookDeliveryListDialog> = TestBed.createComponent(OutgoingWebhookDeliveryListDialog);
  fixture.componentRef.setInput('loadDeliveryPage', loadDeliveryPage);
  fixture.componentRef.setInput('loadDeliveryDetail', loadDeliveryDetail);
  fixture.detectChanges();
  return { fixture, loadDeliveryPage, loadDeliveryDetail };
}

const rows = () => Array.from(document.querySelectorAll<HTMLButtonElement>('.gr-dialog .delivery'));
const footerButton = (label: string) =>
  Array.from(document.querySelectorAll<HTMLButtonElement>('.gr-dialog__footer button')).find((b) => b.textContent?.includes(label))!;

describe('getDeliveryResultReason', () => {
  it('names the HTTP status, else the last cause of the error', () => {
    expect(getDeliveryResultReason({ ...failed, response_status: 500 })).toBe('HTTP 500');
    expect(getDeliveryResultReason(failed)).toBe('connection refused');
    expect(getDeliveryResultReason({ ...failed, error_message: null })).toBeNull();
  });
});

describe('OutgoingWebhookDeliveryListDialog', () => {
  it('names the result, the reason and the event on each row', () => {
    render(() => of([failed, delivered]));
    const [first, second] = rows();
    expect(first.textContent).toContain('Failed');
    expect(first.textContent).toContain('connection refused');
    expect(first.querySelector('.reason')?.getAttribute('title')).toBe(failed.error_message);
    expect(second.textContent).toContain('Delivered');
    expect(second.textContent).toContain('HTTP 200');
  });

  it('opens the error and bodies of a delivery on a row click and closes them on a second click', () => {
    const { fixture, loadDeliveryDetail } = render(() => of([failed]));
    rows()[0].click();
    fixture.detectChanges();
    expect(loadDeliveryDetail).toHaveBeenCalledWith('d1');
    expect(rows()[0].getAttribute('aria-expanded')).toBe('true');
    const detail = document.querySelector('.gr-dialog .detail')!;
    expect(detail.textContent).toContain('connection refused');
    expect(detail.querySelector('pre')?.textContent).toBe('{\n  "event": "build.failed"\n}');
    expect(rows()[0].querySelector('.reason')).toBeNull();
    rows()[0].click();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog .detail')).toBeNull();
  });

  it('names the response status of an expanded delivery and keeps a body that is not JSON as sent', () => {
    const { fixture } = render(
      () => of([{ ...failed, response_status: 404, error_message: null }]),
      (id) => of({ ...failed, id, response_status: 404, error_message: null, request_body: '{}', response_body: 'Not Found' }),
    );
    rows()[0].click();
    fixture.detectChanges();
    const detail = document.querySelector('.gr-dialog .detail')!;
    expect(detail.textContent).toContain('HTTP 404');
    expect(detail.querySelectorAll('pre')[1]?.textContent).toBe('Not Found');
  });

  it('drops a detail that arrives after its row was closed', () => {
    const answer = new Subject<RecordedOutgoingDelivery>();
    const { fixture } = render(() => of([failed]), () => answer);
    rows()[0].click();
    rows()[0].click();
    answer.next({ ...failed, request_body: '{}' });
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog .detail')).toBeNull();
  });

  it('pages past the first 50 deliveries and back', () => {
    const full = Array.from({ length: 50 }, (_, i) => ({ ...delivered, id: `d${i}` }));
    const { fixture, loadDeliveryPage } = render((_, offset) => of(offset === 0 ? full : [failed]));
    expect(loadDeliveryPage).toHaveBeenLastCalledWith(50, 0);
    expect(footerButton('Previous').disabled).toBe(true);
    footerButton('Next').click();
    fixture.detectChanges();
    expect(loadDeliveryPage).toHaveBeenLastCalledWith(50, 50);
    expect(rows()).toHaveLength(1);
    expect(footerButton('Next').disabled).toBe(true);
    footerButton('Previous').click();
    expect(loadDeliveryPage).toHaveBeenLastCalledWith(50, 0);
  });

  it('shows no pager while every delivery fits one page', () => {
    render(() => of([failed]));
    expect(document.querySelectorAll('.gr-dialog__footer button')).toHaveLength(0);
  });

  it('shows a failed load as an error with retry, never as no deliveries', () => {
    let calls = 0;
    const { fixture } = render(() => (++calls === 1 ? throwError(() => new ApiError('database is down', 500)) : of([])));
    expect(document.body.textContent).toContain('Failed to load deliveries');
    expect(document.body.textContent).toContain('database is down');
    expect(document.body.textContent).not.toContain('No deliveries yet');
    document.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    fixture.detectChanges();
    expect(document.body.textContent).toContain('No deliveries yet');
  });
});
