/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, effect, inject, signal, untracked } from '@angular/core';
import { EMPTY, type Observable, catchError, of, tap } from 'rxjs';
import type { YearlyRecapResponse } from '@core/models';
import { ApiService } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import { NotifyService } from '@core/services/notify.service';
import { WhatsNewService } from '@core/whats-new/whats-new.service';

@Injectable({ providedIn: 'root' })
export class YearlyRecapAutoOpenService {
  private api = inject(ApiService);
  private auth = inject(AuthService);
  private notify = inject(NotifyService);
  private whatsNew = inject(WhatsNewService);
  private recapLoadedForUserId: string | null = null;
  private hasAutoOpenedThisRecap = false;
  private hasSentRecapOpenedRequest = false;

  readonly recap = signal<YearlyRecapResponse | null>(null);
  readonly open = signal(false);

  constructor() {
    effect(() => {
      const userId = this.auth.user()?.id ?? null;
      if (userId !== this.recapLoadedForUserId) untracked(() => this.fetchCurrentYearlyRecapAndStoreInRecapSignal(userId));
    });
    effect(() => {
      const recap = this.recap();
      if (!recap || recap.opened || this.hasAutoOpenedThisRecap || this.whatsNew.open()) return;
      this.hasAutoOpenedThisRecap = true;
      this.open.set(true);
    });
  }

  reopenRecapSlideShowFromDashboardCard(): void {
    if (this.recap()) this.open.set(true);
  }

  closeRecapSlideShowAndMarkOpenedOnServer(): void {
    this.open.set(false);
    const recap = this.recap();
    if (!recap || recap.opened || this.hasSentRecapOpenedRequest) return;
    this.hasSentRecapOpenedRequest = true;
    this.api.put<string>('user/yearly-recap').subscribe({
      next: () => this.recap.update((current) => current && { ...current, opened: true }),
      error: (err: unknown) => this.notify.failure("Couldn't mark Gradient Wrapped as opened", err),
    });
  }

  refreshCurrentYearlyRecapAndResetAutoOpenStateOnYearChange(): Observable<unknown> {
    const userId = this.recapLoadedForUserId;
    if (userId === null) return EMPTY;
    return this.api.get<YearlyRecapResponse | null>('user/yearly-recap').pipe(
      tap((recap) => {
        if (this.recapLoadedForUserId !== userId || recap?.year === this.recap()?.year) return;
        this.hasAutoOpenedThisRecap = false;
        this.hasSentRecapOpenedRequest = false;
        this.recap.set(recap ?? null);
        if (!recap) this.open.set(false);
      }),
    );
  }

  private fetchCurrentYearlyRecapAndStoreInRecapSignal(userId: string | null): void {
    this.recapLoadedForUserId = userId;
    this.hasAutoOpenedThisRecap = false;
    this.hasSentRecapOpenedRequest = false;
    this.recap.set(null);
    this.open.set(false);
    if (userId === null) return;
    this.api
      .get<YearlyRecapResponse | null>('user/yearly-recap')
      .pipe(catchError(() => of(null)))
      .subscribe((recap) => {
        if (this.recapLoadedForUserId === userId) this.recap.set(recap ?? null);
      });
  }
}
