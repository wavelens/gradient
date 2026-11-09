/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ConfirmService } from '@shared/confirm';

type VisibilityPlace = 'project' | 'cache';

export interface SaveSettingsWithPrivateToPublicConfirmationInput<T> {
  kind: VisibilityPlace;
  target: string;
  becomesPublic: boolean;
  save: () => Observable<T>;
}

const PUBLIC_CONSEQUENCE: Record<VisibilityPlace, string> = {
  project: "Everyone can view its builds, tasks and logs, even visitors who aren't signed in.",
  cache: "Anyone can download its store paths, even visitors who aren't signed in.",
};

export function injectSaveSettingsWithPrivateToPublicConfirmation(): <T>(
  change: SaveSettingsWithPrivateToPublicConfirmationInput<T>,
) => Observable<T> {
  const confirm = inject(ConfirmService);
  return <T>(change: SaveSettingsWithPrivateToPublicConfirmationInput<T>): Observable<T> => {
    if (!change.becomesPublic) return change.save();
    return confirm.confirm({
      title: `Make ${change.kind} public`,
      target: change.target,
      consequence: PUBLIC_CONSEQUENCE[change.kind],
      confirmLabel: 'Make public',
      failure: "Couldn't save settings",
      severity: 'primary',
      action: change.save,
    });
  };
}
