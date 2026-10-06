/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ChangeDetectionStrategy, afterRenderEffect, computed, inject, signal } from '@angular/core';
import { RouterModule } from '@angular/router';
import { AuthService } from '@core/services/auth.service';

const TABS = [
  { path: 'overview', label: 'Overview' },
  { path: 'live', label: 'Live Jobs' },
  { path: 'scheduler', label: 'Scheduler' },
  { path: 'throughput', label: 'Throughput' },
  { path: 'durations', label: 'Durations' },
  { path: 'workers', label: 'Workers' },
  { path: 'cache', label: 'Cache' },
  { path: 'storage', label: 'Storage', superuser: true },
  { path: 'network', label: 'Network' },
  { path: 'expensive', label: 'Jobs' },
  { path: 'expensive-evals', label: 'Evals' },
  { path: 'health', label: 'System Health', superuser: true },
];

@Component({
  selector: 'app-board-layout',
  standalone: true,
  imports: [RouterModule],
  template: `
    <div class="board">
      <h1>Job Board</h1>
      <nav class="board-nav">
        @for (tab of tabs(); track tab.path) {
          <a #link [routerLink]="tab.path" routerLinkActive="active" (isActiveChange)="$event && activeLink.set(link)">{{ tab.label }}</a>
        }
      </nav>
      <router-outlet></router-outlet>
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './board-layout.component.scss',
})
export class BoardLayoutComponent {
  private auth = inject(AuthService);
  protected tabs = computed(() => {
    const superuser = this.auth.user()?.superuser === true;
    return TABS.filter((tab) => superuser || !tab.superuser);
  });

  protected activeLink = signal<HTMLElement | null>(null);

  constructor() {
    afterRenderEffect(() => {
      this.tabs();
      this.activeLink()?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    });
  }
}
