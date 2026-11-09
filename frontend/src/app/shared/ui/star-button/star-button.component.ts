/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, computed, inject, input, model } from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';
import { finalize } from 'rxjs';
import { NotifyService } from '@core/services/notify.service';
import { StarsService } from '@core/services/stars.service';
import { StarTarget } from '@core/models';
import { ButtonComponent } from '@gradient/ui/ui';

@Component({
  selector: 'gr-star-button',
  standalone: true,
  imports: [ButtonComponent, NgTemplateOutlet],
  template: `
    <ng-template #star>
      <svg class="star gr-button__icon" [class.star--on]="starred()" viewBox="0 0 24 24" aria-hidden="true">
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
        <span class="gr-button__label">{{ starred() ? 'Starred' : 'Star' }}</span>
      </button>
    } @else {
      <button
        type="button"
        class="star-button"
        [attr.aria-pressed]="starred()"
        [attr.aria-label]="'Star ' + name()"
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
  private notify = inject(NotifyService);
  target = input.required<StarTarget>();
  starredItemDisplayName = input<string>();
  starred = model(false);
  labeled = input(false, { transform: booleanAttribute });

  protected name = computed(() => {
    const target = this.target();
    const slug = target.kind === 'cache' ? target.cache : target.kind === 'task' ? `${target.project} / ${target.task}` : target.project;
    return this.starredItemDisplayName() ?? slug;
  });

  private inFlight = false;

  toggle(event: Event): void {
    event.preventDefault();
    event.stopPropagation();
    if (this.inFlight) return;
    const previous = this.starred();
    this.inFlight = true;
    this.starred.set(!previous);
    this.stars
      .set(this.target(), !previous)
      .pipe(finalize(() => (this.inFlight = false)))
      .subscribe({
        error: (error: unknown) => {
          this.starred.set(previous);
          this.notify.failure(`${previous ? "Couldn't unstar" : "Couldn't star"} ${this.name()}`, error);
        },
      });
  }
}
