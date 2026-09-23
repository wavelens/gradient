/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component } from '@angular/core';
import { RouterLink } from '@angular/router';

@Component({
  selector: 'app-dashboard-start',
  standalone: true,
  imports: [RouterLink],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <section>
      <h2>Where do I start?</h2>
      <ol class="steps">
        @for (s of steps; track s.title) {
          <li>
            <div><b>{{ s.title }}</b><span>{{ s.detail }}</span></div>
            <a [routerLink]="s.link">{{ s.action }} -></a>
          </li>
        }
      </ol>
    </section>
  `,
  styleUrl: './dashboard-start.component.scss',
})
export class DashboardStartComponent {
  readonly steps = [
    { title: 'Create a project', detail: 'A project groups tasks and members', action: 'Create', link: '/projects' },
    { title: 'Add a task from a repository', detail: 'Point Gradient at a flake and pick the outputs to build', action: 'Add', link: '/projects' },
    { title: 'Connect a worker', detail: 'Builds run on your own machines', action: 'Connect', link: '/projects' },
    { title: 'Create or subscribe to a cache', detail: 'Serve build results to your machines', action: 'Cache', link: '/caches' },
  ];
}
