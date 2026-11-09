/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { BoardService } from '@core/services/board.service';
import { AuthService } from '@core/services/auth.service';
import { BoardHighCostComponent } from './high-cost.component';

function setup(query: Record<string, string> = {}) {
  TestBed.configureTestingModule({
    imports: [BoardHighCostComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { queryParamMap: convertToParamMap(query) } } },
      {
        provide: BoardService,
        useValue: {
          getExpensive: () => of([]),
          getExpensiveByResource: () => of([]),
          getTopProjects: () => of([]),
          getExpensiveEvalsByResource: () => of([]),
        },
      },
      { provide: AuthService, useValue: { user: () => ({ superuser: false }) } },
    ],
  });
  const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
  const fixture = TestBed.createComponent(BoardHighCostComponent);
  fixture.detectChanges();
  return { fixture, el: fixture.nativeElement as HTMLElement, navigate };
}

describe('BoardHighCostComponent', () => {
  it('ranks builds by default', () => {
    const { el } = setup();
    expect(el.querySelector('app-board-expensive-jobs')).not.toBeNull();
    expect(el.querySelector('app-board-expensive-evals')).toBeNull();
  });

  it('opens the evaluation ranking from the URL', () => {
    expect(setup({ highCostRanking: 'evaluations' }).el.querySelector('app-board-expensive-evals')).not.toBeNull();
  });

  it('drops the build metric when switching to evaluations', () => {
    const { fixture, el, navigate } = setup({ tab: 'ram' });
    (fixture.componentInstance as unknown as { setHighCostRanking(r: string): void }).setHighCostRanking('evaluations');
    fixture.detectChanges();
    expect(el.querySelector('app-board-expensive-evals')).not.toBeNull();
    expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { highCostRanking: 'evaluations', tab: null } }));
  });
});
