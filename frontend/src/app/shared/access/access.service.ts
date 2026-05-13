/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable } from '@angular/core';
import { AccessState } from '@core/models/access.model';

@Injectable({ providedIn: 'root' })
export class AccessService {
  isWritable(s: AccessState): boolean {
    return s.canEdit && !s.managed;
  }

  shouldShowWriteAction(s: AccessState): boolean {
    return s.canEdit;
  }

  shouldDisableInput(s: AccessState): boolean {
    return s.managed || !s.canEdit;
  }

  /// Returns an `AccessState` projected onto trigger-action permissions
  /// (Start Evaluation, Restart Failed Builds, Abort). `canEdit` is replaced
  /// by `canTrigger` and `managed` is forced to `false` — trigger actions
  /// don't mutate config, so the managed flag must not disable them. Pass the
  /// result to `*appWritable` / `[appManagedDisable]` to gate trigger buttons
  /// against the right permission.
  triggerAccess(s: AccessState): AccessState {
    return { managed: false, canEdit: s.canTrigger, canTrigger: s.canTrigger };
  }
}
