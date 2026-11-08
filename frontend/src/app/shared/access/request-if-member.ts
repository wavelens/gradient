/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Observable, throwError } from 'rxjs';
import type { CachePermission, CacheResponse, ProjectResponse } from '@core/models';
import { createMissingPermissionLoadStateError, createNoAccessLoadStateError } from './missing-permission';

export function requestIfProjectMember<T>(
  projectOrTask: Partial<Pick<ProjectResponse, 'role'>> & Pick<ProjectResponse, 'permissions'> | null | undefined,
  request: () => Observable<T>,
): Observable<T> {
  const member = projectOrTask?.role != null || (projectOrTask?.permissions.length ?? 0) > 0;
  return member ? request() : throwError(() => createNoAccessLoadStateError('project', 'members'));
}

export function requestIfCacheMember<T>(
  cache: Pick<CacheResponse, 'permissions'> | null | undefined,
  request: () => Observable<T>,
  permission?: CachePermission,
): Observable<T> {
  if (!cache?.permissions.length) return throwError(() => createNoAccessLoadStateError('cache', 'members'));
  if (permission && !cache.permissions.includes(permission)) {
    return throwError(() => createMissingPermissionLoadStateError(permission, 'cache'));
  }
  return request();
}
