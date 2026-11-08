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

/// Write results appear as toasts. Failed writes stay on screen longer and include the reason from the server. New results replace the previous toast. Later successes leave no old failure toast beside them.
@Injectable({ providedIn: 'root' })
export class NotifyService {
  private messages = inject(MessageService);

  success(summary: string): void {
    this.messages.add({ severity: 'success', summary, key: WRITE_RESULT_TOAST_KEY });
  }

  failure(summary: string, error: unknown): void {
    const detail = error instanceof ApiError ? error.message : undefined;
    this.messages.add({ severity: 'error', summary, detail, life: FAILURE_LIFE_MS, key: WRITE_RESULT_TOAST_KEY });
  }
}
