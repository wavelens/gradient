/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ChangeDetectionStrategy, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router } from '@angular/router';
import { TabSwitchComponent } from '@gradient/ui/ui';
import { oneOf } from '@shared/routing';
import { BoardExpensiveJobsComponent } from '../expensive-jobs/expensive-jobs.component';
import { BoardExpensiveEvalsComponent } from '../expensive-evals/expensive-evals.component';

type Ranking = 'builds' | 'evaluations';

const RANKINGS: { key: Ranking; label: string }[] = [
  { key: 'builds', label: 'Builds' },
  { key: 'evaluations', label: 'Evaluations' },
];

@Component({
  selector: 'app-board-high-cost',
  standalone: true,
  imports: [FormsModule, TabSwitchComponent, BoardExpensiveJobsComponent, BoardExpensiveEvalsComponent],
  template: `
    <gr-tab-switch class="high-cost-ranking" ariaLabel="High-Cost Items" [options]="rankings" optionValue="key"
                   [ngModel]="highCostRanking()" (ngModelChange)="setHighCostRanking($event)" />
    @if (highCostRanking() === 'evaluations') {
      <app-board-expensive-evals />
    } @else {
      <app-board-expensive-jobs />
    }
  `,
  styles: `.high-cost-ranking { display: block; margin-bottom: 1rem; }`,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class BoardHighCostComponent {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  protected readonly rankings = RANKINGS;
  protected highCostRanking = signal<Ranking>(
    oneOf(RANKINGS.map((r) => r.key))(this.route.snapshot.queryParamMap.get('highCostRanking') ?? '') ?? 'builds',
  );

  /// Build rankings and evaluation rankings use different metrics. Tab switches remove the metric that the other ranking selected.
  setHighCostRanking(ranking: Ranking): void {
    this.highCostRanking.set(ranking);
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { highCostRanking: ranking === 'builds' ? null : ranking, tab: null },
      queryParamsHandling: 'merge',
      replaceUrl: true,
    });
  }
}
