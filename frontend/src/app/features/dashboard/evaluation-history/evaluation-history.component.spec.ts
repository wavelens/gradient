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
    f.componentRef.setInput('bars', [bar('Completed', 1000), bar('Failed', 4000), bar('Building', 2000), bar('Waiting', 500), bar('Aborted', 500)]);
    f.componentRef.setInput('project', 'p');
    f.componentRef.setInput('task', 't');
    f.detectChanges();
    const rects = Array.from((f.nativeElement as HTMLElement).querySelectorAll('a.bar'));
    expect(rects.map((r) => (r as HTMLElement).style.background)).toEqual([
      'var(--gr-graph-success)',
      'var(--gr-graph-danger)',
      'var(--gr-graph-running)',
      'var(--gr-status-warning)',
      'var(--gr-text-secondary)',
    ]);
    expect((rects[1] as HTMLElement).style.height).toBe('100%');
    expect(rects[1].getAttribute('href')).toBe('/project/p/task/t?eval=Failed4000');
  });

  it('keeps one Tab stop per row on the newest bar and moves it with the arrow keys', () => {
    TestBed.configureTestingModule({ imports: [EvaluationHistoryComponent], providers: [provideRouter([])] });
    const f = TestBed.createComponent(EvaluationHistoryComponent);
    f.componentRef.setInput('bars', [bar('Completed', 1000), bar('Failed', 4000), bar('Building', 2000)]);
    f.componentRef.setInput('project', 'p');
    f.componentRef.setInput('task', 't');
    f.detectChanges();
    document.body.appendChild(f.nativeElement);
    const root = f.nativeElement as HTMLElement;
    const tabIndexes = () => Array.from(root.querySelectorAll('a.bar')).map((r) => r.getAttribute('tabindex'));
    expect(tabIndexes()).toEqual(['-1', '-1', '0']);
    const history = root.querySelector('.history') as HTMLElement;
    history.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
    f.detectChanges();
    expect(tabIndexes()).toEqual(['-1', '0', '-1']);
    expect(document.activeElement).toBe(root.querySelectorAll('a.bar')[1]);
    history.dispatchEvent(new KeyboardEvent('keydown', { key: 'Home', bubbles: true }));
    history.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
    f.detectChanges();
    expect(tabIndexes()).toEqual(['0', '-1', '-1']);
    f.nativeElement.remove();
  });

  it('draws plain bars without links or tab stops inside another link', () => {
    TestBed.configureTestingModule({ imports: [EvaluationHistoryComponent], providers: [provideRouter([])] });
    const f = TestBed.createComponent(EvaluationHistoryComponent);
    f.componentRef.setInput('bars', [bar('Completed', 1000), bar('Failed', 4000)]);
    f.componentRef.setInput('project', 'p');
    f.componentRef.setInput('task', 't');
    f.componentRef.setInput('barsAreLinks', false);
    f.detectChanges();
    const root = f.nativeElement as HTMLElement;
    expect(root.querySelectorAll('a')).toHaveLength(0);
    expect(Array.from(root.querySelectorAll('span.bar')).map((b) => b.getAttribute('title'))).toEqual(['Completed · 1.0 s', 'Failed · 4.0 s']);
  });
});
