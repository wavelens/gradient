/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, effect, inject, signal, untracked } from '@angular/core';
import { defaultIfEmpty, take } from 'rxjs';
import { ButtonComponent, DialogComponent } from '@gradient/ui/ui';
import { apiErrorMessage } from '@core/services/api.service';
import { ConfirmService } from './confirm.service';

@Component({
  selector: 'app-confirm-dialog',
  standalone: true,
  imports: [ButtonComponent, DialogComponent],
  template: `
    @let current = confirm.pending();
    <gr-dialog
      [header]="current?.request?.title ?? ''"
      [visible]="current !== null"
      (visibleChange)="$event || cancel()"
      [busy]="running()"
      [error]="error()"
      width="440px"
    >
      @if (current; as pending) {
        <p><strong>{{ pending.request.target }}</strong></p>
        <p>{{ pending.request.consequence }}@if (pending.request.isIrreversible) { This action is permanent.}</p>
      }
      <div grDialogFooter>
        <button grButton label="Cancel" severity="secondary" [disabled]="running()" (click)="cancel()"></button>
        <button grButton [label]="current?.request?.confirmLabel ?? ''" [severity]="current?.request?.severity ?? 'danger'"
                [loading]="running()" (click)="run()"></button>
      </div>
    </gr-dialog>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ConfirmDialogComponent {
  protected confirm = inject(ConfirmService);
  protected running = signal(false);
  protected error = signal<string | null>(null);

  constructor() {
    effect(() => {
      this.confirm.pending();
      untracked(() => this.error.set(null));
    });
  }

  protected cancel(): void {
    if (this.running()) return;
    const pending = this.confirm.pending();
    this.confirm.pending.set(null);
    pending?.settle(null);
  }

  protected run(): void {
    const pending = this.confirm.pending();
    if (!pending || this.running()) return;
    this.running.set(true);
    this.error.set(null);
    pending.request.action().pipe(take(1), defaultIfEmpty(undefined)).subscribe({
      next: (value) => {
        this.running.set(false);
        this.confirm.pending.set(null);
        pending.settle({ value });
      },
      error: (error: unknown) => {
        this.running.set(false);
        this.error.set(apiErrorMessage(error, pending.request.failure));
      },
    });
  }
}
