/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, computed, effect, inject, signal, untracked } from '@angular/core';
import { Observable, tap } from 'rxjs';
import { PendingUserDecisionsResponse } from '@core/models';
import { AuthService } from './auth.service';
import { UserService } from './user.service';

const NO_PENDING_DECISIONS: PendingUserDecisionsResponse = { invites: [], subscription_requests: [], team_requests: [] };

@Injectable({ providedIn: 'root' })
export class PendingUserDecisionsService {
  private users = inject(UserService);
  private auth = inject(AuthService);
  private decisions = signal<PendingUserDecisionsResponse>(NO_PENDING_DECISIONS);

  readonly all = this.decisions.asReadonly();
  readonly invites = computed(() => this.decisions().invites);
  readonly count = computed(() => {
    const { invites, subscription_requests, team_requests } = this.decisions();
    return invites.length + subscription_requests.length + team_requests.length;
  });

  constructor() {
    effect(() => {
      const signedIn = this.auth.isAuthenticated();
      untracked(() => (signedIn ? this.refresh() : this.decisions.set(NO_PENDING_DECISIONS)));
    });
  }

  fetchAndStorePendingDecisions(): Observable<PendingUserDecisionsResponse> {
    return this.users.getRequestsAwaitingUserDecision().pipe(tap((decisions) => this.decisions.set(decisions)));
  }

  refresh(): void {
    this.fetchAndStorePendingDecisions().subscribe({ error: () => undefined });
  }

  removeHandledItemsFromPendingDecisions(update: (decisions: PendingUserDecisionsResponse) => PendingUserDecisionsResponse): void {
    this.decisions.update(update);
  }
}
