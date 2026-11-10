/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Observable } from 'rxjs';
import { AdminService } from '@core/services/admin.service';
import { ConfirmService } from '@shared/confirm';

export function confirmAndStartDeepGcForAllStorageBackends(confirm: ConfirmService, admin: AdminService): Observable<unknown> {
  return confirm.confirm({
    title: 'Run Deep GC',
    target: 'All storage backends',
    consequence: 'Deep GC runs delete stored files that no row in the database references. A round already in progress can start again from the beginning.',
    confirmLabel: 'Run Deep GC',
    failure: 'Failed to start deep GC.',
    severity: 'danger',
    action: () => admin.startDeepGc(),
  });
}
