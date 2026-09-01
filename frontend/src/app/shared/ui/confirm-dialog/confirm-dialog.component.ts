/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject, ChangeDetectionStrategy } from '@angular/core';
import { IconComponent } from '../icon/icon.component';
import { ButtonComponent } from '../button/button.component';
import { ConfirmationService } from '../confirmation/confirmation.service';
import { DialogComponent } from '../dialog/dialog.component';

@Component({
  selector: 'gr-confirm-dialog',
  standalone: true,
  imports: [IconComponent, ButtonComponent, DialogComponent],
  template: `
    <gr-dialog
      [visible]="!!pending()"
      (visibleChange)="$event || confirmations.reject()"
      [header]="pending()?.header ?? 'Confirm'"
      width="420px"
    >
      <div class="gr-confirm">
        @if (pending()?.icon) {
          <gr-icon [name]="pending()?.icon ?? ''" size="xl" class="gr-confirm__icon" />
        }
        <p class="gr-confirm__message">{{ pending()?.message }}</p>
      </div>
      <div grDialogFooter>
        <button
          grButton
          [label]="rejectProps().label ?? 'Cancel'"
          [severity]="rejectProps().severity ?? 'secondary'"
          (click)="confirmations.reject()"
        ></button>
        <button
          grButton
          [label]="acceptProps().label ?? 'Yes'"
          [severity]="acceptProps().severity ?? 'primary'"
          (click)="confirmations.accept()"
        ></button>
      </div>
    </gr-dialog>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './confirm-dialog.component.scss',
})
export class ConfirmDialogComponent {
  protected confirmations = inject(ConfirmationService);
  protected pending = this.confirmations.pending;
  protected acceptProps = computed(() => this.pending()?.acceptButtonProps ?? {});
  protected rejectProps = computed(() => this.pending()?.rejectButtonProps ?? {});
}
