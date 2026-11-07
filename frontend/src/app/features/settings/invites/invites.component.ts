/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute } from '@angular/router';
import { UserService } from '@core/services/user.service';
import { NotifyService } from '@core/services/notify.service';
import { InviteItem } from '@core/models';
import {
  BadgeComponent,
  ButtonComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';

@Component({
  selector: 'app-invites',
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
  templateUrl: './invites.component.html',
  styleUrl: './invites.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class InvitesComponent implements OnInit {
  private userService = inject(UserService);
  private route = inject(ActivatedRoute);
  private notify = inject(NotifyService);

  loading = signal(true);
  pendingToken = signal<string | null>(null);
  invites = signal<InviteItem[]>([]);
  loadError = signal<unknown>(null);

  ngOnInit(): void {
    this.load();

    const token = this.route.snapshot.queryParamMap.get('token');
    if (token) {
      this.accept(token);
    }
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.userService.getInvites().subscribe({
      next: (invites) => {
        this.invites.set(invites);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  accept(token: string): void {
    this.pendingToken.set(token);
    this.userService.acceptInvite(token).subscribe({
      next: () => {
        this.pendingToken.set(null);
        this.notify.success('Invitation accepted');
        this.load();
      },
      error: (err: unknown) => {
        this.pendingToken.set(null);
        this.notify.failure('Invitation accept failed', err);
      },
    });
  }

  decline(token: string): void {
    this.pendingToken.set(token);
    this.userService.declineInvite(token).subscribe({
      next: () => {
        this.pendingToken.set(null);
        this.notify.success('Invitation declined');
        this.load();
      },
      error: (err: unknown) => {
        this.pendingToken.set(null);
        this.notify.failure('Invitation decline failed', err);
      },
    });
  }
}
