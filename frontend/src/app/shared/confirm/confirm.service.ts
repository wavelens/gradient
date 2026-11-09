/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, signal } from '@angular/core';
import { Observable, Subscriber } from 'rxjs';
import type { ButtonSeverity } from '@gradient/ui/ui';

export interface ConfirmRequest<T = unknown> {
  title: string;
  target: string;
  consequence: string;
  confirmLabel: string;
  failure: string;
  severity?: ButtonSeverity;
  isIrreversible?: boolean;
  action: () => Observable<T>;
}

export interface PendingConfirm {
  request: ConfirmRequest;
  settle: (result: { value: unknown } | null) => void;
}

/// Ask for confirmation before a destructive action. Run that action in the opened dialog. The returned observable can emit the result of the action on success. That observable can complete with no value when users cancel.
@Injectable({ providedIn: 'root' })
export class ConfirmService {
  readonly pending = signal<PendingConfirm | null>(null);

  confirm<T>(request: ConfirmRequest<T>): Observable<T> {
    return new Observable<T>((subscriber: Subscriber<T>) => {
      this.pending()?.settle(null);
      this.pending.set({
        request: request as ConfirmRequest,
        settle: (result) => {
          if (result) subscriber.next(result.value as T);
          subscriber.complete();
        },
      });
      return () => {
        if (this.pending()?.request === request) this.pending.set(null);
      };
    });
  }
}
