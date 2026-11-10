/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input, model } from '@angular/core';

export interface CheckGroup {
  label: string;
  items: readonly { value: string; label: string; description?: string; disabled?: boolean }[];
}

@Component({
  selector: 'app-check-groups',
  standalone: true,
  template: `
    <div class="groups">
      @for (group of groups(); track group.label) {
        <fieldset [disabled]="disabled()">
          <legend>{{ group.label }}</legend>
          @for (item of group.items; track item.value) {
            <label>
              <input type="checkbox"
                     [checked]="selected().includes(item.value)"
                     [disabled]="item.disabled"
                     (change)="toggle(item.value, $any($event.target).checked)" />
              <span class="item-text">
                {{ item.label }}
                @if (item.description) {
                  <small>{{ item.description }}</small>
                }
              </span>
            </label>
          }
        </fieldset>
      }
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './check-groups.component.scss',
})
export class CheckGroupsComponent {
  groups = input.required<readonly CheckGroup[]>();
  selected = model.required<string[]>();
  disabled = input(false);

  protected toggle(value: string, checked: boolean): void {
    const set = new Set(this.selected());
    if (checked) set.add(value);
    else set.delete(value);
    this.selected.set(Array.from(set));
  }
}
