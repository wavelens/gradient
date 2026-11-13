/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, output, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { NgTemplateOutlet } from '@angular/common';
import { RouterLink } from '@angular/router';
import { Observable, filter, tap } from 'rxjs';
import { DashboardService } from '@core/services/dashboard.service';
import { StarsService } from '@core/services/stars.service';
import { EvaluationStatus, Rail, RailProject } from '@core/models';
import { BadgeComponent, IconComponent, SkeletonComponent, TooltipDirective } from '@gradient/ui/ui';
import { StarButtonComponent, formatEntityLabel } from '@shared/ui';
import { formatCount, plural } from '@shared/text';
import { PHASE_COLOR, evaluationPhase, getEvaluationStatusLabel } from '@shared/evaluation';
import { links } from '@shared/routing';

const maxRowsPerDashboardRailCard = 5;

function starredFirstEntriesLimitedToMaxRowsPerCard<T extends { starred: boolean }>(items: readonly T[]): T[] {
  return [...items].sort((a, b) => Number(b.starred) - Number(a.starred)).slice(0, maxRowsPerDashboardRailCard);
}

@Component({
  selector: 'app-dashboard-rail',
  standalone: true,
  imports: [NgTemplateOutlet, RouterLink, BadgeComponent, IconComponent, SkeletonComponent, StarButtonComponent, TooltipDirective],
  changeDetection: ChangeDetectionStrategy.Eager,
  templateUrl: './dashboard-rail.component.html',
  styleUrl: './dashboard-rail.component.scss',
})
export class DashboardRailComponent implements OnInit {
  protected readonly links = links;
  protected readonly formatEntityLabel = formatEntityLabel;
  protected readonly maxRowsPerDashboardRailCard = maxRowsPerDashboardRailCard;
  private dashboard = inject(DashboardService);
  rail = signal<Rail | null>(null);
  protected projectsCardEntries = computed(() => starredFirstEntriesLimitedToMaxRowsPerCard(this.rail()?.projects ?? []));
  protected cachesCardEntries = computed(() => starredFirstEntriesLimitedToMaxRowsPerCard(this.rail()?.caches ?? []));
  failed = signal(false);
  hidden = signal(false);
  empty = output<boolean>();

  readonly count = formatCount;

  evaluationStatusDotColor(status: EvaluationStatus | null | undefined): string | null {
    return status ? PHASE_COLOR[evaluationPhase(status)] : null;
  }

  statusDotTooltipText(status: EvaluationStatus | null | undefined): string {
    return status ? getEvaluationStatusLabel(status) : 'No evaluations yet';
  }

  getProjectRowLinkTooltipText(project: RailProject): string {
    return `${formatEntityLabel(project)} · ${plural(project.task_count, 'task')}`;
  }

  getPackageCountTooltipText(narCount: number): string {
    return `${narCount.toLocaleString('en')} ${narCount === 1 ? 'package' : 'packages'}`;
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
