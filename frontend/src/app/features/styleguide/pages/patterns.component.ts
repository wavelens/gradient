/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ChangeDetectionStrategy } from '@angular/core';
import {
  BadgeComponent,
  ButtonComponent,
  CardGridComponent,
  Crumb,
  FieldRowComponent,
  FormFieldComponent,
  InputDirective,
  NavCardComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SettingsSectionComponent,
} from '@shared/ui';

@Component({
  selector: 'app-sg-patterns',
  standalone: true,
  imports: [
    PageLayoutComponent, RowListComponent, RowComponent, CardGridComponent,
    SettingsSectionComponent, FormFieldComponent, FieldRowComponent, ButtonComponent,
    BadgeComponent, InputDirective,
    NavCardComponent,
  ],
  templateUrl: './patterns.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './patterns.component.scss',
})
export class PatternsComponent {
  crumbs: Crumb[] = [
    { label: 'my-project', link: ['/styleguide'] },
    { label: 'Settings', link: ['/styleguide'] },
    { label: 'Integrations' },
  ];
  cards = [
    { name: 'gradient', status: 'passing', severity: 'success' as const },
    { name: 'nixpkgs-mirror', status: 'failed', severity: 'danger' as const },
    { name: 'infra', status: 'queued', severity: 'neutral' as const },
  ];
}
