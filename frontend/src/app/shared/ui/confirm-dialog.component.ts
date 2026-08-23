/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject } from '@angular/core';
import { ButtonComponent } from './button.component';
import { ConfirmationService } from './confirmation.service';
import { DialogComponent } from './dialog.component';

@Component({
  selector: 'gr-confirm-dialog',
  standalone: true,
  imports: [ButtonComponent, DialogComponent],
  template: `
    <gr-dialog
      [visible]="!!pending()"
      (visibleChange)="$event || confirmations.reject()"
      [header]="pending()?.header ?? 'Confirm'"
      width="420px"
    >
      <div class="gr-confirm">
        @if (pending()?.icon) {
          <span class="material-symbols-outlined gr-confirm__icon" aria-hidden="true">{{ pending()?.icon }}</span>
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
  styles: [
    `
      .gr-confirm { display: flex; align-items: flex-start; gap: 0.75rem; }
      .gr-confirm__icon { color: #ffc107; font-size: 24px; }
      .gr-confirm__message { margin: 0; }
    `,
  ],
})
export class ConfirmDialogComponent {
  protected confirmations = inject(ConfirmationService);
  protected pending = this.confirmations.pending;
  protected acceptProps = computed(() => this.pending()?.acceptButtonProps ?? {});
  protected rejectProps = computed(() => this.pending()?.rejectButtonProps ?? {});
}
