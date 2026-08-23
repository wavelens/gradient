/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, signal } from '@angular/core';
import { ButtonSeverity } from './button.component';

export interface ConfirmButton {
  label?: string;
  severity?: ButtonSeverity;
}

export interface Confirmation {
  message?: string;
  header?: string;
  icon?: string;
  acceptButtonProps?: ConfirmButton;
  rejectButtonProps?: ConfirmButton;
  accept?: () => void;
  reject?: () => void;
}

@Injectable()
export class ConfirmationService {
  private current = signal<Confirmation | null>(null);
  readonly pending = this.current.asReadonly();

  confirm(confirmation: Confirmation): void {
    this.current.set(confirmation);
  }

  accept(): void {
    const c = this.current();
    this.current.set(null);
    c?.accept?.();
  }

  reject(): void {
    const c = this.current();
    this.current.set(null);
    c?.reject?.();
  }
}
