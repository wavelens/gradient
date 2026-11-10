/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, DOCUMENT, effect, inject, signal, untracked, ChangeDetectionStrategy } from '@angular/core';
import { MessageService } from '../message/message.service';
import { MessageSeverity } from '../types';

const TOAST_GAP_PX = 8;

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
    <div class="gr-toast" role="status" aria-live="polite" [style.top.px]="toastStackTopOffsetPx()">
      @for (message of messages(); track message.id) {
        <div class="gr-toast__item gr-toast__item--{{ message.severity }}" [attr.role]="message.severity === 'error' ? 'alert' : null">
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
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './toast.component.scss',
})
export class ToastComponent {
  protected messageService = inject(MessageService);
  protected messages = this.messageService.messages;
  private document = inject(DOCUMENT);
  protected readonly toastStackTopOffsetPx = signal<number | null>(null);
  private shownCount = 0;

  constructor() {
    effect(() => {
      const count = this.messages().length;
      if (count > this.shownCount) untracked(() => this.toastStackTopOffsetPx.set(this.computeViewportYJustBelowPageTitleHeaderOrNull()));
      this.shownCount = count;
    });
  }

  private computeViewportYJustBelowPageTitleHeaderOrNull(): number | null {
    const pageHeader = this.document.querySelector('.page-layout__header');
    const appHeader = this.document.querySelector('.main-header');
    if (!pageHeader) return null;
    const below = pageHeader.getBoundingClientRect().bottom + TOAST_GAP_PX;
    const appHeaderBottom = appHeader?.getBoundingClientRect().bottom ?? 0;
    return below > appHeaderBottom + TOAST_GAP_PX ? below : null;
  }

  protected iconFor(severity: MessageSeverity | undefined): string {
    return ICONS[severity ?? 'info'];
  }
}
