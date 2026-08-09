/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input, output } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ACTION_EVENTS } from '../../../core/models/action.model';

@Component({
  selector: 'app-action-events',
  standalone: true,
  imports: [CommonModule],
  templateUrl: './action-events.component.html',
  styleUrl: './action-events.component.scss',
})
export class ActionEventsComponent {
  selected = input.required<string[]>();
  disabled = input(false);
  selectedChange = output<string[]>();

  readonly grouped = computed(() => {
    const byGroup = new Map<string, typeof ACTION_EVENTS>();
    for (const e of ACTION_EVENTS) {
      if (!byGroup.has(e.group)) byGroup.set(e.group, []);
      byGroup.get(e.group)!.push(e);
    }
    return Array.from(byGroup.entries()).map(([group, items]) => ({ group, items }));
  });

  toggle(value: string, checked: boolean) {
    const set = new Set(this.selected());
    if (checked) set.add(value); else set.delete(value);
    this.selectedChange.emit(Array.from(set));
  }
}
