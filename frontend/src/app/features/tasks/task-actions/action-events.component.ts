/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject, input, output, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { toSignal } from '@angular/core/rxjs-interop';
import { groupCatalog } from '@core/models';
import { EventsService } from '@core/services/events.service';

@Component({
  selector: 'app-action-events',
  standalone: true,
  imports: [CommonModule],
  templateUrl: './action-events.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './action-events.component.scss',
})
export class ActionEventsComponent {
  selected = input.required<string[]>();
  disabled = input(false);
  families = input<string[] | undefined>(undefined);
  selectedChange = output<string[]>();

  private readonly catalog = toSignal(inject(EventsService).catalog$, { initialValue: [] });

  readonly grouped = computed(() => groupCatalog(this.catalog(), this.families()));

  toggle(value: string, checked: boolean) {
    const set = new Set(this.selected());
    if (checked) set.add(value); else set.delete(value);
    this.selectedChange.emit(Array.from(set));
  }
}
