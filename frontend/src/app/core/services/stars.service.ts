/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { StarTarget } from '@core/models';

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

@Injectable({ providedIn: 'root' })
export class StarsService {
  private api = inject(ApiService);

  set(target: StarTarget, starred: boolean): Observable<boolean> {
    return starred
      ? this.api.put<boolean>(starPath(target), {})
      : this.api.delete<boolean>(starPath(target));
  }
}
