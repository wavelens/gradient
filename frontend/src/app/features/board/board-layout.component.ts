/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ChangeDetectionStrategy, computed, inject } from '@angular/core';
import { RouterOutlet } from '@angular/router';
import { LinkTabsComponent, type LinkTab } from '@gradient/ui/ui';
import { AuthService } from '@core/services/auth.service';
import { links, type BoardTab } from '@shared/routing';

const TABS: { path: BoardTab; label: string; superuser?: boolean }[] = [
  { path: 'overview', label: 'Overview' },
  { path: 'live', label: 'Live Jobs' },
  { path: 'scheduler', label: 'Scheduler', superuser: true },
  { path: 'throughput', label: 'Throughput' },
  { path: 'durations', label: 'Durations' },
  { path: 'workers', label: 'Workers' },
  { path: 'cache', label: 'Cache' },
  { path: 'storage', label: 'Storage', superuser: true },
  { path: 'network', label: 'Network' },
  { path: 'expensive', label: 'High Cost' },
  { path: 'health', label: 'System Health', superuser: true },
];

@Component({
  selector: 'app-board-layout',
  standalone: true,
  imports: [RouterOutlet, LinkTabsComponent],
  template: `
    <div class="board">
      <h1>Job Board</h1>
      <gr-link-tabs class="board-nav" [tabs]="tabs()" ariaLabel="Pages of Job Board" />
      <router-outlet></router-outlet>
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './board-layout.component.scss',
})
export class BoardLayoutComponent {
  private auth = inject(AuthService);
  protected tabs = computed<LinkTab[]>(() => {
    const superuser = this.auth.user()?.superuser === true;
    return TABS.filter((tab) => superuser || !tab.superuser).map((tab) => ({ label: tab.label, link: links.board(tab.path) }));
  });
}
