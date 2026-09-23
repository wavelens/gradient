/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component } from '@angular/core';
import { CardGridComponent, NavCardComponent } from '@shared/ui';

@Component({
  selector: 'app-dashboard-start',
  standalone: true,
  imports: [CardGridComponent, NavCardComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <section class="section">
      <h2>Where do I start?</h2>
      <gr-card-grid min="240px">
        @for (s of steps; track s.title) {
          <gr-nav-card [icon]="s.icon" [title]="s.title" [description]="s.detail" [link]="[s.link]" />
        }
      </gr-card-grid>
    </section>
  `,
})
export class DashboardStartComponent {
  readonly steps = [
    { icon: 'folder', title: 'Create a project', detail: 'A project groups tasks and members', link: '/projects' },
    { icon: 'add_task', title: 'Add a task from a repository', detail: 'Point Gradient at a flake and pick the outputs to build', link: '/projects' },
    { icon: 'dns', title: 'Connect a worker', detail: 'Builds run on your own machines', link: '/projects' },
    { icon: 'storage', title: 'Create or subscribe to a cache', detail: 'Serve build results to your machines', link: '/caches' },
  ];
}
