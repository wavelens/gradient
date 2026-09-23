/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, output, signal } from '@angular/core';
import { RouterLink } from '@angular/router';
import { DashboardService } from '@core/services/dashboard.service';
import { Rail } from '@core/models';
import { ButtonComponent, StarButtonComponent } from '@shared/ui';
import { formatCount } from '@shared/text';

@Component({
  selector: 'app-dashboard-rail',
  standalone: true,
  imports: [RouterLink, ButtonComponent, StarButtonComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  templateUrl: './dashboard-rail.component.html',
  styleUrl: './dashboard-rail.component.scss',
})
export class DashboardRailComponent implements OnInit {
  private dashboard = inject(DashboardService);
  rail = signal<Rail | null>(null);
  failed = signal(false);
  empty = output<boolean>();

  readonly count = formatCount;
  readonly operations = [
    { label: 'Job Board', link: '/board' },
    { label: 'Workers', link: '/board/workers' },
    { label: 'Scheduler', link: '/board/scheduler' },
    { label: 'Health', link: '/board/health' },
  ];

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.failed.set(false);
    this.dashboard.rail().subscribe({
      next: (r) => {
        this.rail.set(r);
        this.empty.emit(r.projects.length === 0 && r.caches.length === 0);
      },
      error: (e: { status?: number }) => {
        this.failed.set(e?.status !== 403);
        this.empty.emit(false);
      },
    });
  }
}
