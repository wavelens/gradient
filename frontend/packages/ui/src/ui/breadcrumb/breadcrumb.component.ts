/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { RouterLink, type UrlTree } from '@angular/router';
import { SkeletonComponent } from '../skeleton/skeleton.component';

// A `null` label is a name still loading; it shows as a placeholder.
export interface Crumb {
  label: string | null;
  link?: unknown[] | UrlTree;
}

@Component({
  selector: 'gr-breadcrumb',
  standalone: true,
  imports: [RouterLink, SkeletonComponent],
  template: `
    <nav class="breadcrumb" aria-label="Breadcrumb">
      @for (crumb of trail(); track $index; let last = $last) {
        @if (last || !crumb.link) {
          <span class="breadcrumb-current" [attr.aria-current]="last ? 'page' : null">
            @if (crumb.label === null) { <gr-skeleton width="8ch" /> } @else { {{ crumb.label }} }
          </span>
        } @else {
          <a class="breadcrumb-link" [routerLink]="crumb.link">
            @if (crumb.label === null) { <gr-skeleton width="8ch" /> } @else { {{ crumb.label }} }
          </a>
        }
        @if (!last) { <span class="breadcrumb-separator" aria-hidden="true">/</span> }
      }
    </nav>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class BreadcrumbComponent {
  trail = input.required<Crumb[]>();
}
