/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable, of } from 'rxjs';
import { catchError, map, switchMap } from 'rxjs/operators';
import { ApiService } from './api.service';
import { AuthService } from './auth.service';
import { StarTarget, UserStars } from '@core/models';

export function starPath(t: StarTarget): string {
  switch (t.kind) {
    case 'project':
      return `user/stars/projects/${encodeURIComponent(t.project)}`;
    case 'task':
      return `user/stars/tasks/${encodeURIComponent(t.project)}/${encodeURIComponent(t.task)}`;
    case 'cache':
      return `user/stars/caches/${encodeURIComponent(t.cache)}`;
  }
}

export function isStarred(stars: UserStars, t: StarTarget): boolean {
  switch (t.kind) {
    case 'project':
      return stars.projects.includes(t.project);
    case 'task':
      return stars.tasks.some((s) => s.project === t.project && s.task === t.task);
    case 'cache':
      return stars.caches.includes(t.cache);
  }
}

@Injectable({ providedIn: 'root' })
export class StarsService {
  private api = inject(ApiService);
  private auth = inject(AuthService);

  list(): Observable<UserStars> {
    return this.api.get<UserStars>('user/stars');
  }

  /// Guests have no stars, and a failed lookup renders as unstarred.
  starred(target: StarTarget): Observable<boolean> {
    return this.auth.initialized$.pipe(
      switchMap(() => (this.auth.isAuthenticated() ? this.list() : of(null))),
      map((stars) => !!stars && isStarred(stars, target)),
      catchError(() => of(false)),
    );
  }

  set(target: StarTarget, starred: boolean): Observable<boolean> {
    return starred
      ? this.api.put<boolean>(starPath(target), {})
      : this.api.delete<boolean>(starPath(target));
  }
}
