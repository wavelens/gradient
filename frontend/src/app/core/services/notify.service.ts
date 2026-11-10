/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { MessageService } from '@gradient/ui/ui';
import { ApiError } from './api.service';

const FAILURE_LIFE_MS = 8000;
const WRITE_RESULT_TOAST_KEY = 'writeResultToastKey';

/// Write results appear as toasts. Failed writes stay on the screen for a long time and include the reason from the server. New results replace the previous toast. Later successes leave no old failure toast next to the new toast. No toast can appear on session expiry (401). The error interceptor can already send the user to the sign-in page.
@Injectable({ providedIn: 'root' })
export class NotifyService {
  private messages = inject(MessageService);

  success(summary: string): void {
    this.messages.add({ severity: 'success', summary, key: WRITE_RESULT_TOAST_KEY });
  }

  failure(summary: string, error: unknown): void {
    if (error instanceof ApiError && error.status === 401) return;
    const detail = error instanceof ApiError ? error.message : undefined;
    this.messages.add({ severity: 'error', summary, detail, life: FAILURE_LIFE_MS, key: WRITE_RESULT_TOAST_KEY });
  }

  copyToClipboard(text: string, thing: string): Promise<void> {
    const written = navigator.clipboard?.writeText(text) ?? Promise.reject(new Error('Clipboard unavailable'));
    return written.then(
      () => this.success(`${thing.charAt(0).toUpperCase()}${thing.slice(1)} copied`),
      (error: unknown) => this.failure(`Couldn't copy ${thing}`, error),
    );
  }
}
