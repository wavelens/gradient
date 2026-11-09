/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
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

function setup() {
  TestBed.configureTestingModule({
    imports: [BoardSchedulerComponent],
    providers: [
      {
        provide: BoardService,
        useValue: { query: () => of([]), getScoringSummary: () => of(SUMMARY), getScoringRules: () => of([]) },
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
});
