/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, inject, ChangeDetectionStrategy } from '@angular/core';
import { ConfigService } from '@core/services/config.service';

@Component({
  selector: 'app-footer',
  standalone: true,
  templateUrl: './footer.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './footer.component.scss',
})
export class FooterComponent {
  config = inject(ConfigService);
}
