/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { CanActivateFn } from '@angular/router';
import { ConfigService } from '@core/services/config.service';
import { authGuard } from './auth.guard';

export const publicListingGuard: CanActivateFn = (route, state) =>
  inject(ConfigService).publicListing || authGuard(route, state);
