/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input, model } from '@angular/core';
import { permissionLabelWithCapitalizedFirstLetter } from '@shared/text';
import { PermissionEntry } from '@core/models';
import { CheckGroupsComponent, type CheckGroup } from '../check-groups/check-groups.component';

@Component({
  selector: 'app-permission-picker',
  standalone: true,
  imports: [CheckGroupsComponent],
  template: `<app-check-groups [groups]="groups()" [selected]="selected()" (selectedChange)="select($event)" />`,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class PermissionPickerComponent {
  permissions = input.required<readonly PermissionEntry[]>();
  selection = model<Record<string, boolean>>({});
  selectablePermissionIds = input<readonly string[] | null>(null);

  private selectable(id: string): boolean {
    return this.selectablePermissionIds()?.includes(id) ?? true;
  }

  protected groups = computed<CheckGroup[]>(() => {
    const group = (label: string, mutating: boolean): CheckGroup => ({
      label,
      items: this.permissions()
        .filter((p) => p.mutating === mutating)
        .map((p) => ({ value: p.id, label: permissionLabelWithCapitalizedFirstLetter(p.id), disabled: !this.selectable(p.id) })),
    });
    return [group('Read', false), group('Write', true)].filter((g) => g.items.length > 0);
  });
  protected selected = computed(() => Object.keys(this.selection()).filter((id) => this.selection()[id] && this.selectable(id)));

  protected select(ids: string[]): void {
    this.selection.set(Object.fromEntries(this.permissions().map((p) => [p.id, ids.includes(p.id)])));
  }
}
