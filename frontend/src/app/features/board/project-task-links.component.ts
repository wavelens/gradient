/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { RouterLink } from '@angular/router';
import { links } from '@shared/routing';

@Component({
  selector: 'app-board-project-task-links',
  standalone: true,
  imports: [RouterLink],
  template: `
    @if (project(); as project) {
      <a [routerLink]="links.project(project)" (click)="$event.stopPropagation()">{{ projectDisplayName() || project }}</a>
      @if (task(); as task) {
        / <a [routerLink]="links.task(project, task, evaluation())" (click)="$event.stopPropagation()">{{ taskDisplayName() || task }}</a>
      }
    } @else {
      <span class="muted">-</span>
    }
  `,
  styles: `
    :host { overflow-wrap: anywhere; }
    a { color: inherit; text-decoration: none; }
    a:hover { color: var(--gr-status-info); text-decoration: underline; }
    .muted { color: var(--gr-text-muted); }
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class BoardProjectTaskLinksComponent {
  protected readonly links = links;
  readonly project = input<string | null | undefined>();
  readonly projectDisplayName = input<string | null | undefined>();
  readonly task = input<string | null | undefined>();
  readonly taskDisplayName = input<string | null | undefined>();
  readonly evaluation = input<string | null | undefined>();
}
