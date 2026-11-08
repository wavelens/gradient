/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Observable, throwError } from 'rxjs';
import type { ProjectResponse } from '@core/models';
import { createNoAccessLoadStateError } from './missing-permission';

export function requestIfProjectMember<T>(
  project: Pick<ProjectResponse, 'role'> | null | undefined,
  request: () => Observable<T>,
): Observable<T> {
  return project?.role != null ? request() : throwError(() => createNoAccessLoadStateError('project', 'members'));
}
