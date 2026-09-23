/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, inject, input, model } from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';
import { StarsService } from '@core/services/stars.service';
import { StarTarget } from '@core/models';
import { ButtonComponent } from '../button/button.component';

@Component({
  selector: 'gr-star-button',
  standalone: true,
  imports: [ButtonComponent, NgTemplateOutlet],
  template: `
    <ng-template #star>
      <svg class="star" [class.star--on]="starred()" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 2.5l2.9 6.1 6.6.8-4.9 4.6 1.3 6.6L12 17.3l-5.9 3.3 1.3-6.6-4.9-4.6 6.6-.8z" />
      </svg>
    </ng-template>
    @if (labeled()) {
      <button
        type="button"
        grButton
        severity="secondary"
        [attr.aria-pressed]="starred()"
        (click)="toggle($event)"
      >
        <ng-container [ngTemplateOutlet]="star" />
        {{ starred() ? 'Starred' : 'Star' }}
      </button>
    } @else {
      <button
        type="button"
        class="star-button"
        [attr.aria-pressed]="starred()"
        [attr.aria-label]="starred() ? 'Unstar' : 'Star'"
        (click)="toggle($event)"
      >
        <ng-container [ngTemplateOutlet]="star" />
      </button>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './star-button.component.scss',
})
export class StarButtonComponent {
  private stars = inject(StarsService);
  target = input.required<StarTarget>();
  starred = model(false);
  labeled = input(false, { transform: booleanAttribute });

  toggle(event: Event): void {
    event.preventDefault();
    event.stopPropagation();
    const next = !this.starred();
    this.starred.set(next);
    this.stars.set(this.target(), next).subscribe({ error: () => this.starred.set(!next) });
  }
}
