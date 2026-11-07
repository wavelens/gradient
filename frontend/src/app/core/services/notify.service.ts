/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { MessageService } from '@gradient/ui/ui';
import { ApiError } from './api.service';

const FAILURE_LIFE_MS = 8000;

/// Report write results as toasts. A failed write can stay on screen longer and include the reason from the server.
@Injectable({ providedIn: 'root' })
export class NotifyService {
  private messages = inject(MessageService);

  success(summary: string): void {
    this.messages.add({ severity: 'success', summary });
  }

  failure(summary: string, error: unknown): void {
    const detail = error instanceof ApiError ? error.message : undefined;
    this.messages.add({ severity: 'error', summary, detail, life: FAILURE_LIFE_MS });
  }
}
