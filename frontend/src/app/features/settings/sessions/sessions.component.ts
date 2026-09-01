/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { UserService } from '@core/services/user.service';
import { Session } from '@core/models';
import {
  BadgeComponent,
  ButtonComponent,
  EmptyStateComponent,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@shared/ui';

@Component({
  selector: 'app-sessions',
  standalone: true,
  imports: [
    CommonModule,
    BadgeComponent,
    ButtonComponent,
    EmptyStateComponent,
    LoadingSpinnerComponent,
    MessageBannerComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
  ],
  templateUrl: './sessions.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class SessionsComponent implements OnInit {
  private userService = inject(UserService);

  loading = signal(true);
  revokingId = signal<string | null>(null);
  sessions = signal<Session[]>([]);
  errorMessage = signal<string | null>(null);

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.userService.getSessions().subscribe({
      next: (sessions) => {
        this.sessions.set(sessions);
        this.loading.set(false);
      },
      error: () => {
        this.errorMessage.set('Failed to load sessions.');
        this.loading.set(false);
      },
    });
  }

  revoke(s: Session): void {
    if (s.current) {
      return;
    }
    this.revokingId.set(s.id);
    this.userService.revokeSession(s.id).subscribe({
      next: () => {
        this.revokingId.set(null);
        this.load();
      },
      error: () => {
        this.revokingId.set(null);
        this.errorMessage.set('Failed to revoke session.');
      },
    });
  }
}
