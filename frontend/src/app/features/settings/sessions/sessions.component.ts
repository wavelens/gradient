/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { UserService } from '@core/services/user.service';
import { NotifyService } from '@core/services/notify.service';
import { SessionInfo } from '@core/models';
import { deviceName } from '@shared/text';
import { ConfirmService } from '@shared/confirm';
import {
  BadgeComponent,
  ButtonComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';
import { links } from '@shared/routing';

@Component({
  selector: 'app-sessions',
  standalone: true,
  imports: [
    CommonModule,
    BadgeComponent,
    ButtonComponent,
    LoadStateComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
  ],
  templateUrl: './sessions.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class SessionsComponent implements OnInit {
  protected readonly links = links;
  private userService = inject(UserService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  protected readonly deviceName = deviceName;

  loading = signal(true);
  sessions = signal<SessionInfo[]>([]);
  loadError = signal<unknown>(null);

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.userService.getSessions().subscribe({
      next: (sessions) => {
        this.sessions.set(sessions);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  revoke(s: SessionInfo): void {
    if (s.current) {
      return;
    }
    this.confirm
      .confirm({
        title: 'Revoke session',
        target: `${deviceName(s.user_agent)} (${s.ip || 'Unknown IP'})`,
        consequence: 'Sessions on that device end. Sign in again to use the device.',
        confirmLabel: 'Revoke',
        failure: 'Failed to revoke the session.',
        action: () => this.userService.revokeSession(s.id),
      })
      .subscribe(() => {
        this.notify.success('Session revoked');
        this.load();
      });
  }
}
