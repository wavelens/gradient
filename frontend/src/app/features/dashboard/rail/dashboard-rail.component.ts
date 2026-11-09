/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, output, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { RouterLink } from '@angular/router';
import { Observable, filter, tap } from 'rxjs';
import { DashboardService } from '@core/services/dashboard.service';
import { StarsService } from '@core/services/stars.service';
import { EvaluationStatus, Rail } from '@core/models';
import { SkeletonComponent } from '@gradient/ui/ui';
import { StarButtonComponent } from '@shared/ui';
import { formatCount } from '@shared/text';
import { PHASE_COLOR, evaluationPhase } from '@shared/evaluation';
import { links } from '@shared/routing';

@Component({
  selector: 'app-dashboard-rail',
  standalone: true,
  imports: [RouterLink, SkeletonComponent, StarButtonComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  templateUrl: './dashboard-rail.component.html',
  styleUrl: './dashboard-rail.component.scss',
})
export class DashboardRailComponent implements OnInit {
  protected readonly links = links;
  private dashboard = inject(DashboardService);
  rail = signal<Rail | null>(null);
  failed = signal(false);
  hidden = signal(false);
  empty = output<boolean>();

  readonly count = formatCount;

  evaluationStatusDotColor(status: EvaluationStatus | null | undefined): string | null {
    return status ? PHASE_COLOR[evaluationPhase(status)] : null;
  }

  constructor() {
    inject(StarsService).changed$
      .pipe(filter((target) => target.kind !== 'task'), takeUntilDestroyed())
      .subscribe(() => this.load());
  }

  ngOnInit(): void {
    this.load();
  }

  reloadSilentlyKeepingContentOnError(): Observable<Rail> {
    return this.dashboard.rail().pipe(
      tap((r) => {
        this.rail.set(r);
        this.failed.set(false);
        this.empty.emit(r.projects.length === 0);
      }),
    );
  }

  load(): void {
    this.failed.set(false);
    this.reloadSilentlyKeepingContentOnError().subscribe({
      error: (e: { status?: number }) => {
        this.hidden.set(e?.status === 403);
        this.failed.set(e?.status !== 403);
        this.empty.emit(false);
      },
    });
  }
}
