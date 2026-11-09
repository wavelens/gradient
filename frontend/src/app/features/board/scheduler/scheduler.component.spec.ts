/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Observable, of, throwError } from 'rxjs';
import { BoardService } from '@core/services/board.service';
import type { ScoringSummary } from '@core/models';
import { BoardSchedulerComponent } from './scheduler.component';

const SUMMARY: ScoringSummary = {
  sample_size: 12,
  score_min: 902.04,
  score_max: 7600.4,
  score_avg: 1234.567,
  histogram: [],
  rules: [{ rule: 'Locality', avg: 1.234, min: 0.5, max: 2.25 }],
};

function setup(summary: Observable<ScoringSummary> = of(SUMMARY)) {
  TestBed.configureTestingModule({
    imports: [BoardSchedulerComponent],
    providers: [
      {
        provide: BoardService,
        useValue: { query: () => of([]), getScoringSummary: () => summary, getScoringRules: () => of([]) },
      },
    ],
  });
  const fixture = TestBed.createComponent(BoardSchedulerComponent);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

describe('BoardSchedulerComponent', () => {
  it('shows whole numbers in the Min / Max tile so it fits a narrow screen', () => {
    expect(setup().querySelector('.kpis')?.textContent).toContain('902 / 7,600');
  });

  it('labels the score histogram with whole numbers', () => {
    setup(of({ ...SUMMARY, histogram: [{ lo: 906.04, hi: 1463.8, count: 3 }, { lo: 1463.8, hi: 2021.6, count: 1 }] }));
    const component = TestBed.createComponent(BoardSchedulerComponent).componentInstance;
    component.ngOnInit();
    expect(component.histogramCategories()).toEqual(['906', '1,464']);
  });

  it('prints scores with one decimal and calls the bar column Share like the job page', () => {
    const el = setup();
    expect(el.querySelector('.kpis')?.textContent).toContain('1,234.6');
    expect(el.querySelector('.rules tbody td.num')?.textContent?.trim()).toBe('1.2');
    expect(el.querySelector('.rules thead')?.textContent).toContain('Share');
  });

  it('says the summary failed instead of showing zero assignments', () => {
    const el = setup(throwError(() => new Error('Internal server error')));
    const cards = Array.from(el.querySelectorAll('.kpis gr-stat-card')).map((c) => c.textContent ?? '');
    expect(cards.every((text) => text.includes("Couldn't load"))).toBe(true);
    expect(el.querySelector('.rules tbody')?.textContent).toContain("Couldn't load");
    expect(el.querySelector('gr-skeleton')).toBeNull();
  });
});
