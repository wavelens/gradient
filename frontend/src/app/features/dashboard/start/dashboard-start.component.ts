/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component } from '@angular/core';
import { RowComponent, RowListComponent } from '@gradient/ui/ui';

@Component({
  selector: 'app-dashboard-start',
  standalone: true,
  imports: [RowComponent, RowListComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <section class="section">
      <h2>Where do I start?</h2>
      <gr-row-list>
        @for (s of steps; track s.title) {
          <gr-row [icon]="s.icon" [linkLabel]="s.title" [link]="[s.link]">
            {{ s.title }}
            <span slot="meta">{{ s.detail }}</span>
          </gr-row>
        }
      </gr-row-list>
    </section>
  `,
})
export class DashboardStartComponent {
  readonly steps = [
    { icon: 'folder', title: 'Create a project', detail: 'A project groups tasks and members', link: '/projects' },
    { icon: 'group', title: 'Create a team', detail: 'A team shares its members and workers with projects and caches', link: '/teams' },
    { icon: 'storage', title: 'Create or subscribe to a cache', detail: 'Serve build results to your machines', link: '/caches' },
  ];
}
