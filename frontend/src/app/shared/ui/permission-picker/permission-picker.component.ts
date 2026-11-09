/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, computed, input, model } from '@angular/core';
import { permissionLabelWithCapitalizedFirstLetter } from '@shared/text';
import { getLowestBuiltInRoleForPermissionWireName } from '@shared/access';
import { PermissionEntry } from '@core/models';
import { CheckGroupsComponent, type CheckGroup } from '../check-groups/check-groups.component';

const viewPermissionIds = new Set(['viewProject', 'viewCache']);

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
  viewPermissionIsRequired = input(false, { transform: booleanAttribute });

  private selectable(id: string): boolean {
    return this.selectablePermissionIds()?.includes(id) ?? true;
  }

  private locked(id: string): boolean {
    return this.viewPermissionIsRequired() && viewPermissionIds.has(id);
  }

  protected groups = computed<CheckGroup[]>(() => {
    const group = (label: string, role: 'View' | 'Write' | 'Admin'): CheckGroup => ({
      label,
      items: this.permissions()
        .filter((p) => getLowestBuiltInRoleForPermissionWireName(p.id) === role)
        .map((p) => ({ value: p.id, label: permissionLabelWithCapitalizedFirstLetter(p.id), disabled: !this.selectable(p.id) || this.locked(p.id) })),
    });
    return [group('Read', 'View'), group('Write', 'Write'), group('Admin', 'Admin')].filter((g) => g.items.length > 0);
  });
  protected selected = computed(() =>
    this.permissions()
      .map((p) => p.id)
      .filter((id) => this.locked(id) || (this.selection()[id] && this.selectable(id))),
  );

  protected select(ids: string[]): void {
    this.selection.set(Object.fromEntries(this.permissions().map((p) => [p.id, ids.includes(p.id) || this.locked(p.id)])));
  }
}
