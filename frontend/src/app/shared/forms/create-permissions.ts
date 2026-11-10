/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, computed, inject } from '@angular/core';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';

export function injectCanCreateProject(): Signal<boolean> {
  const auth = inject(AuthService);
  const config = inject(ConfigService);
  return computed(() => auth.isAuthenticated() && config.canCreate(config.createProject, auth.user()?.superuser === true));
}

export function injectCanCreateCache(): Signal<boolean> {
  const auth = inject(AuthService);
  const config = inject(ConfigService);
  return computed(() =>
    auth.isAuthenticated()
    && auth.user()?.can_create_cache !== false
    && config.canCreate(config.createCache, auth.user()?.superuser === true));
}

export function injectCanCreateTeam(): Signal<boolean> {
  const auth = inject(AuthService);
  const config = inject(ConfigService);
  return computed(() => auth.isAuthenticated() && config.canCreate(config.createTeam, auth.user()?.superuser === true));
}
