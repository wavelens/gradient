/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, InjectionToken, effect, inject, signal, untracked } from '@angular/core';
import { environment } from '@environments/environment';
import { ApiService } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import type { UserInfoResponse } from '@core/models';
import { RELEASE_NOTES, type ReleaseNotes } from './release-notes.generated';
import { compareVersions, releaseLine } from './version';

export const RELEASE_NOTES_SOURCE = new InjectionToken<readonly ReleaseNotes[]>('release notes', {
  factory: () => RELEASE_NOTES,
});

function releasedNotes(current: string, notes: readonly ReleaseNotes[]): ReleaseNotes[] {
  return notes
    .filter((n) => compareVersions(n.version, releaseLine(current)) <= 0)
    .sort((a, b) => compareVersions(b.version, a.version));
}

@Injectable({ providedIn: 'root' })
export class WhatsNewService {
  private api = inject(ApiService);
  private auth = inject(AuthService);
  private checkedUser: string | null = null;
  private seenVersion: string | null = null;

  readonly current = environment.version;
  private released = releasedNotes(this.current, inject(RELEASE_NOTES_SOURCE));
  readonly hasNotes = this.released.length > 0;
  readonly open = signal(false);
  readonly notes = signal<readonly ReleaseNotes[]>([]);

  constructor() {
    effect(() => {
      const user = this.auth.user();
      if (user && user.id !== this.checkedUser) untracked(() => this.check(user));
    });
  }

  showAll(): void {
    this.notes.set(this.released);
    this.open.set(true);
  }

  close(): void {
    this.open.set(false);
    if (this.seenVersion !== this.current) this.markSeen();
  }

  private check(user: UserInfoResponse): void {
    this.checkedUser = user.id;
    this.seenVersion = user.last_seen_version ?? null;
    const seen = this.seenVersion;
    if (seen === null) {
      this.markSeen();
      return;
    }
    const unseen = this.released.filter((n) => compareVersions(n.version, releaseLine(seen)) > 0);
    if (unseen.length === 0) return;
    this.notes.set(unseen);
    this.open.set(true);
  }

  private markSeen(): void {
    this.seenVersion = this.current;
    this.api.put<string>('user/whats-new', { version: this.current }).subscribe({ error: () => undefined });
  }
}
