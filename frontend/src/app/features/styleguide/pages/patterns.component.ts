/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal, ChangeDetectionStrategy } from '@angular/core';
import { FormsModule } from '@angular/forms';
import {
  BadgeComponent,
  ButtonComponent,
  CardGridComponent,
  IconComponent,
  Crumb,
  FieldRowComponent,
  FormFieldComponent,
  InputDirective,
  NavCardComponent,
  SelectComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { BuildProgressComponent, InputFetchListComponent } from '@shared/ui';
import type { BuildProgress, InputFetch } from '@core/models';

@Component({
  selector: 'app-sg-patterns',
  standalone: true,
  imports: [
    PageLayoutComponent, RowListComponent, RowComponent, CardGridComponent,
    SettingsSectionComponent, FormFieldComponent, FieldRowComponent, ButtonComponent,
    BadgeComponent, InputDirective, FormsModule,
    NavCardComponent, SelectComponent, IconComponent, InputFetchListComponent, BuildProgressComponent,
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
  role = signal('admin');
  roleOptions = [
    { label: 'Admin', value: 'admin' },
    { label: 'Viewer', value: 'viewer' },
  ];
  cards = [
    { name: 'gradient', status: 'passing', severity: 'success' as const },
    { name: 'nixpkgs-mirror', status: 'failed', severity: 'danger' as const },
    { name: 'infra', status: 'queued', severity: 'neutral' as const },
  ];
  inputFetches: InputFetch[] = [
    { name: 'nixpkgs', state: 'Fetching', downloaded_bytes: 18_400_000, expected_bytes: 46_000_000 },
    { name: 'home-manager', state: 'Fetching', downloaded_bytes: 2_100_000, expected_bytes: 0 },
    { name: 'flake-utils', state: 'Done', downloaded_bytes: 15_300, expected_bytes: 0 },
    { name: 'crane', state: 'Queued', downloaded_bytes: 0, expected_bytes: 0 },
    { name: 'private-overlay', state: 'Failed', downloaded_bytes: 0, expected_bytes: 0 },
  ];
  prefetch: BuildProgress = { phase: 'prefetch', bytes_done: 126_000_000, bytes_total: 356_000_000, paths_done: 12, paths_total: 40 };
  upload: BuildProgress = { phase: 'upload', bytes_done: 0, bytes_total: null, paths_done: 1, paths_total: 3 };
  download: BuildProgress = { phase: 'download', bytes_done: 18_400_000, bytes_total: 46_000_000, paths_done: 0, paths_total: 1 };
}
