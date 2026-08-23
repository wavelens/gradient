/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, model, output, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ButtonComponent, DialogComponent } from '@shared/ui';

@Component({
  selector: 'gr-form-dialog',
  standalone: true,
  imports: [CommonModule, ButtonComponent, DialogComponent],
  templateUrl: './form-dialog.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './form-dialog.component.scss',
})
export class FormDialogComponent {
  visible = model<boolean>(false);
  title = input<string>('');
  submitLabel = input<string>('Save');
  cancelLabel = input<string>('Cancel');
  submitIcon = input<string>();
  submitSeverity = input<string>();
  loading = input<boolean>(false);
  disabled = input<boolean>(false);
  width = input<string>('420px');

  submit = output<void>();
  cancel = output<void>();

  onSubmit(): void {
    this.submit.emit();
  }

  onCancel(): void {
    this.cancel.emit();
    this.visible.set(false);
  }
}
