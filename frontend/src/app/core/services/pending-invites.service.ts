/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, computed, effect, inject, signal, untracked } from '@angular/core';
import { InviteItem } from '@core/models';
import { AuthService } from './auth.service';
import { UserService } from './user.service';

@Injectable({ providedIn: 'root' })
export class PendingInvitesService {
  private users = inject(UserService);
  private auth = inject(AuthService);
  private invites = signal<InviteItem[]>([]);

  readonly all = this.invites.asReadonly();
  readonly count = computed(() => this.invites().length);

  constructor() {
    effect(() => {
      const signedIn = this.auth.isAuthenticated();
      untracked(() => (signedIn ? this.refresh() : this.invites.set([])));
    });
  }

  refresh(): void {
    this.users.getInvites().subscribe({
      next: (invites) => this.invites.set(invites),
      error: () => undefined,
    });
  }

  show(invites: InviteItem[]): void {
    this.invites.set(invites);
  }
}
