/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { EvaluationHistoryComponent } from './evaluation-history.component';
import { HistoryBar } from '@core/models';

const bar = (status: HistoryBar['status'], duration_ms: number): HistoryBar => ({
  id: status + duration_ms,
  status,
  duration_ms,
  created_at: '2026-09-23T10:00:00',
});

describe('EvaluationHistoryComponent', () => {
  it('draws one bar per evaluation, colored by status, tallest for the slowest', () => {
    TestBed.configureTestingModule({ imports: [EvaluationHistoryComponent], providers: [provideRouter([])] });
    const f = TestBed.createComponent(EvaluationHistoryComponent);
    f.componentRef.setInput('bars', [bar('Completed', 1000), bar('Failed', 4000), bar('Building', 2000)]);
    f.componentRef.setInput('project', 'p');
    f.componentRef.setInput('task', 't');
    f.detectChanges();
    const rects = Array.from((f.nativeElement as HTMLElement).querySelectorAll('a.bar'));
    expect(rects.map((r) => r.className)).toEqual(['bar bar--ok', 'bar bar--fail', 'bar bar--run']);
    expect((rects[1] as HTMLElement).style.height).toBe('100%');
    expect(rects[1].getAttribute('href')).toBe('/project/p/task/t?eval=Failed4000');
  });
});
