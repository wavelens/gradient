/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, inject, input, model, output, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Observable, Subscription, catchError, interval, of, switchMap } from 'rxjs';
import { ButtonComponent, DialogComponent, FormFieldComponent, InputDirective, MessageBannerComponent } from '@gradient/ui/ui';
import { ConnectionStatus, GradientCiScope, connectWaitState, gradientCiConnectUrl } from '@core/models';
import { ConfigService } from '@core/services/config.service';
import { WorkersService } from '@core/services/workers.service';

const POLL_MS = 3000;
const TEAM_CONNECTED =
  "Connected. Gradient.CI Servers is dialed once a project grants this team's workers.";

@Component({
  selector: 'app-gradient-ci-connect',
  standalone: true,
  imports: [FormsModule, ButtonComponent, DialogComponent, FormFieldComponent, InputDirective, MessageBannerComponent],
  templateUrl: './gradient-ci-connect.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class GradientCiConnectComponent {
  private config = inject(ConfigService);
  private workersService = inject(WorkersService);

  visible = model(false);
  scope = input.required<GradientCiScope>();
  team = input<string | undefined>(undefined);
  project = input<string | undefined>(undefined);
  label = input.required<string>();
  statusOf = input.required<(workerId: string) => Observable<ConnectionStatus | undefined>>();
  changed = output<void>();

  token = '';
  submitting = signal(false);
  waiting = signal(false);
  errorMessage = signal<string | null>(null);
  notice = signal<string | null>(null);
  private poll: Subscription | null = null;

  constructor() {
    inject(DestroyRef).onDestroy(() => this.stopWaiting());
  }

  openShop(): void {
    window.open(
      gradientCiConnectUrl(this.config.gradientCiUrl, this.scope(), this.label()),
      'gradient-ci-connect',
      'popup,width=520,height=760',
    );
  }

  submit(): void {
    const token = this.token.trim();
    if (!token || this.submitting() || this.waiting() || this.notice()) return;
    this.submitting.set(true);
    this.errorMessage.set(null);
    const request = { scope: this.scope(), team: this.team(), project: this.project(), token };
    this.workersService.connectGradientCi(request).subscribe({
      next: (res) => {
        this.submitting.set(false);
        if (this.scope() === 'team') {
          this.token = '';
          this.notice.set(TEAM_CONNECTED);
          this.changed.emit();
          return;
        }
        this.waitFor(res.worker_id);
      },
      error: (err) => {
        this.submitting.set(false);
        this.errorMessage.set(err?.message || 'Failed to connect.');
      },
    });
  }

  onVisibleChange(visible: boolean): void {
    if (visible) this.visible.set(true);
    else this.close();
  }

  close(): void {
    this.stopWaiting();
    this.token = '';
    this.errorMessage.set(null);
    this.notice.set(null);
    this.visible.set(false);
  }

  private waitFor(workerId: string): void {
    this.waiting.set(true);
    this.changed.emit();
    const started = Date.now();
    this.poll = interval(POLL_MS)
      .pipe(switchMap(() => this.statusOf()(workerId).pipe(catchError(() => of(undefined)))))
      .subscribe((status) => this.onStatus(status, Date.now() - started));
  }

  private onStatus(status: ConnectionStatus | undefined, elapsedMs: number): void {
    const state = connectWaitState(status, elapsedMs);
    if (state === 'waiting') return;
    this.changed.emit();
    if (state === 'online') {
      this.close();
      return;
    }
    this.stopWaiting();
    this.errorMessage.set(status?.last_error?.reason ?? 'Gradient.CI Servers did not connect within 30 s.');
  }

  private stopWaiting(): void {
    this.poll?.unsubscribe();
    this.poll = null;
    this.waiting.set(false);
  }
}
