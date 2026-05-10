/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component } from '@angular/core';
import { RouterOutlet } from '@angular/router';
import { AccessBannerComponent } from '@shared/access';
import { injectCacheAccess } from '@core/resolvers/inject-access';

@Component({
  selector: 'app-cache-layout',
  standalone: true,
  imports: [RouterOutlet, AccessBannerComponent],
  templateUrl: './cache-layout.component.html',
  styleUrl: './cache-layout.component.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class CacheLayoutComponent {
  access = injectCacheAccess();
}
