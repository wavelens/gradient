/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input, model } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { BadgeComponent, CheckboxComponent } from '@gradient/ui/ui';
import { permissionLabel } from '@shared/text';

export interface PickablePermission {
  id: string;
  mutating: boolean;
}

@Component({
  selector: 'app-permission-picker',
  standalone: true,
  imports: [FormsModule, BadgeComponent, CheckboxComponent],
  template: `
    <div class="permissions-list">
      @for (perm of permissions(); track perm.id) {
        <div class="permission-row">
          <gr-checkbox
            [inputId]="'perm-' + perm.id"
            [label]="label(perm.id)"
            [binary]="true"
            [ngModel]="!!selection()[perm.id]"
            (ngModelChange)="set(perm.id, $event)"
          />
          <gr-badge [severity]="perm.mutating ? 'warning' : 'success'">
            {{ perm.mutating ? 'mutating' : 'read-only' }}
          </gr-badge>
        </div>
      }
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './permission-picker.component.scss',
})
export class PermissionPickerComponent {
  permissions = input.required<readonly PickablePermission[]>();
  selection = model<Record<string, boolean>>({});

  protected readonly label = permissionLabel;

  protected set(id: string, on: boolean): void {
    this.selection.set({ ...this.selection(), [id]: on });
  }
}
