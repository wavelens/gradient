/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component } from '@angular/core';
import { RouterOutlet } from '@angular/router';

@Component({
  selector: 'app-cache-layout',
  standalone: true,
  imports: [RouterOutlet],
  templateUrl: './cache-layout.component.html',
  styleUrl: './cache-layout.component.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class CacheLayoutComponent {}
