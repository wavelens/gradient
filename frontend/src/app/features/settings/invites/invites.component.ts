/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute } from '@angular/router';
import { UserService } from '@core/services/user.service';
import { Invite } from '@core/models';
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
  selector: 'app-invites',
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
  templateUrl: './invites.component.html',
  styleUrl: './invites.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class InvitesComponent implements OnInit {
  private userService = inject(UserService);
  private route = inject(ActivatedRoute);

  loading = signal(true);
  pendingToken = signal<string | null>(null);
  invites = signal<Invite[]>([]);
  errorMessage = signal<string | null>(null);
  successMessage = signal<string | null>(null);

  ngOnInit(): void {
    this.load();

    const token = this.route.snapshot.queryParamMap.get('token');
    if (token) {
      this.accept(token);
    }
  }

  load(): void {
    this.loading.set(true);
    this.userService.getInvites().subscribe({
      next: (invites) => {
        this.invites.set(invites);
        this.loading.set(false);
      },
      error: () => {
        this.errorMessage.set('Failed to load invitations.');
        this.loading.set(false);
      },
    });
  }

  accept(token: string): void {
    this.pendingToken.set(token);
    this.userService.acceptInvite(token).subscribe({
      next: () => {
        this.pendingToken.set(null);
        this.successMessage.set('Invitation accepted.');
        this.load();
      },
      error: (e: Error) => {
        this.pendingToken.set(null);
        this.errorMessage.set(e.message || 'Failed to accept the invitation.');
      },
    });
  }

  decline(token: string): void {
    this.pendingToken.set(token);
    this.userService.declineInvite(token).subscribe({
      next: () => {
        this.pendingToken.set(null);
        this.successMessage.set('Invitation declined.');
        this.load();
      },
      error: (e: Error) => {
        this.pendingToken.set(null);
        this.errorMessage.set(e.message || 'Failed to decline the invitation.');
      },
    });
  }
}
