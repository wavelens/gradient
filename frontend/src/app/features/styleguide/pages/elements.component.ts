/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, inject, ChangeDetectionStrategy } from '@angular/core';
import {
  BadgeComponent,
  BadgeSeverity,
  ButtonComponent,
  CardGridComponent,
  CopyFieldComponent,
  DividerComponent,
  EmptyStateComponent,
  EvalStatusBadgeComponent,
  FieldRowComponent,
  IconComponent,
  IconSize,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  MessageService,
  MetricChartComponent,
  StatCardComponent,
  TableComponent,
  ToastComponent,
} from '@shared/ui';

@Component({
  selector: 'app-sg-elements',
  standalone: true,
  imports: [
    BadgeComponent, CopyFieldComponent, FieldRowComponent, IconComponent,
    MessageBannerComponent, EmptyStateComponent, LoadingSpinnerComponent,
    StatCardComponent, TableComponent, DividerComponent, EvalStatusBadgeComponent,
    MetricChartComponent, ToastComponent, ButtonComponent,
    CardGridComponent,
  ],
  providers: [MessageService],
  templateUrl: './elements.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './demo.scss',
})
export class ElementsComponent {
  private messages = inject(MessageService);

  evalStatuses = [
    'Queued', 'Fetching', 'EvaluatingFlake', 'EvaluatingDerivation',
    'Building', 'Waiting', 'Completed', 'Failed', 'Aborted',
  ] as const;
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
