/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, inject } from '@angular/core';
import { MessageService } from './message.service';
import { MessageSeverity } from './types';

const ICONS: Record<MessageSeverity, string> = {
  success: 'check_circle',
  info: 'info',
  warn: 'warning',
  error: 'error',
};

@Component({
  selector: 'gr-toast',
  standalone: true,
  template: `
    <div class="gr-toast" role="status" aria-live="polite">
      @for (message of messages(); track message.id) {
        <div class="gr-toast__item gr-toast__item--{{ message.severity }}">
          <span class="material-symbols-outlined" aria-hidden="true">{{ iconFor(message.severity) }}</span>
          <div class="gr-toast__text">
            <strong>{{ message.summary }}</strong>
            @if (message.detail) { <span>{{ message.detail }}</span> }
          </div>
          <button type="button" class="gr-toast__close" aria-label="Dismiss" (click)="messageService.remove(message.id)">
            <span class="material-symbols-outlined" aria-hidden="true">close</span>
          </button>
        </div>
      }
    </div>
  `,
  styleUrl: './toast.component.scss',
})
export class ToastComponent {
  protected messageService = inject(MessageService);
  protected messages = this.messageService.messages;

  protected iconFor(severity: MessageSeverity | undefined): string {
    return ICONS[severity ?? 'info'];
  }
}
