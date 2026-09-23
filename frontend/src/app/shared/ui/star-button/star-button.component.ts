/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, inject, input, model } from '@angular/core';
import { StarsService } from '@core/services/stars.service';
import { StarTarget } from '@core/models';
import { IconComponent } from '../icon/icon.component';

@Component({
  selector: 'gr-star-button',
  standalone: true,
  imports: [IconComponent],
  template: `
    <button
      type="button"
      class="star-button"
      [class.star-button--on]="starred()"
      [attr.aria-pressed]="starred()"
      [attr.aria-label]="starred() ? 'Unstar' : 'Star'"
      (click)="toggle($event)"
    >
      <gr-icon name="star" size="sm" />
    </button>
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
  styleUrl: './star-button.component.scss',
})
export class StarButtonComponent {
  private stars = inject(StarsService);
  target = input.required<StarTarget>();
  starred = model(false);

  toggle(event: Event): void {
    event.preventDefault();
    event.stopPropagation();
    const next = !this.starred();
    this.starred.set(next);
    this.stars.set(this.target(), next).subscribe({ error: () => this.starred.set(!next) });
  }
}
