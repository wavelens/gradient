/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { of } from 'rxjs';
import { StarsService } from '@core/services/stars.service';
import {
  BadgeComponent,
  BadgeSeverity,
  ButtonComponent,
  CardGridComponent,
  CopyFieldComponent,
  DividerComponent,
  EmptyStateComponent,
  FieldRowComponent,
  IconComponent,
  IconSize,
  LoadingSpinnerComponent,
  LogoComponent,
  MessageBannerComponent,
  MessageService,
  StatCardComponent,
  TableComponent,
  ToastComponent,
} from '@gradient/ui/ui';
import {
  EvalStatusBadgeComponent,
  InputFetchListComponent,
  MetricChartComponent,
  SegmentedBarComponent,
  StarButtonComponent,
  StatusIconComponent,
} from '@shared/ui';
import type { InputFetch } from '@core/models';
import type { StatusPhase } from '@shared/evaluation';

@Component({
  selector: 'app-sg-elements',
  standalone: true,
  imports: [
    BadgeComponent, CopyFieldComponent, FieldRowComponent, IconComponent,
    MessageBannerComponent, EmptyStateComponent, LoadingSpinnerComponent,
    StatCardComponent, TableComponent, DividerComponent, EvalStatusBadgeComponent,
    MetricChartComponent, ToastComponent, ButtonComponent,
    CardGridComponent,
    LogoComponent, StarButtonComponent, StatusIconComponent, InputFetchListComponent, SegmentedBarComponent,
  ],
  // The demo star toggles locally instead of writing the viewer's real stars.
  providers: [MessageService, { provide: StarsService, useValue: { set: () => of(true) } }],
  templateUrl: './elements.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrls: ['./demo.scss', './elements.component.scss'],
})
export class ElementsComponent {
  private messages = inject(MessageService);

  evalStatuses = [
    'Queued', 'Fetching', 'EvaluatingFlake', 'EvaluatingDerivation',
    'Building', 'Waiting', 'Completed', 'Failed', 'Aborted',
  ] as const;
  statusPhases: StatusPhase[] = ['queued', 'waiting', 'running', 'success', 'failure', 'aborted'];
  statusPhase = signal<StatusPhase>('queued');
  inputFetches: InputFetch[] = [
    { name: 'nixpkgs', state: 'Fetching', downloaded_bytes: 18_400_000, expected_bytes: 46_000_000 },
    { name: 'home-manager', state: 'Fetching', downloaded_bytes: 2_100_000, expected_bytes: 0 },
    { name: 'flake-utils', state: 'Done', downloaded_bytes: 15_300, expected_bytes: 0 },
    { name: 'crane', state: 'Queued', downloaded_bytes: 0, expected_bytes: 0 },
    { name: 'private-overlay', state: 'Failed', downloaded_bytes: 0, expected_bytes: 0 },
  ];
  chartSeries = [{ name: 'Completed', data: [12, 18, 9, 24, 21] }];
  chartCategories = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri'];

  toast(): void {
    this.messages.add({ severity: 'success', summary: 'Saved', detail: 'Settings updated.' });
  }

  severities: BadgeSeverity[] = ['neutral', 'success', 'danger', 'warning', 'info'];
  iconSizes: IconSize[] = ['sm', 'md', 'xl'];
  storePath = '/nix/store/9k3m1x0a4b2c-hello-2.12.1';
  publicKey = [
    'cache.gradient.example-1:8Xk2mQ9vR4tL6nW3pY7sD1fH5jK0aZcVbNmQwErTyUi=',
    'cache.gradient.example-2:3Jd8sK1mP5qX9wZ2vB6nR4tY7uI0oL3aS5dF8gH1jK2=',
  ].join('\n');
  rows = [
    { name: 'gradient', status: 'Active', updated: '2 hours ago' },
    { name: 'nixpkgs-mirror', status: 'Failed', updated: 'yesterday' },
  ];
}
