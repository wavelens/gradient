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
  RowComponent,
  RowListComponent,
  SkeletonComponent,
  StatCardComponent,
  TableComponent,
  ToastComponent,
} from '@gradient/ui/ui';
import {
  AllowedCapabilitiesComponent,
  EvalStatusBadgeComponent,
  type BarSegment,
  MetricChartComponent,
  PermissionPickerComponent,
  SegmentedBarComponent,
  StarButtonComponent,
  StatusIconComponent,
  byteSegments,
} from '@shared/ui';
import type { AllowedCapabilities, BuildStatusCounts } from '@core/models';
import type { StatusPhase } from '@shared/evaluation';

function counts(c: Partial<BuildStatusCounts>): BuildStatusCounts {
  return { completed: 0, failed: 0, building: 0, queued: 0, substituted: 0, aborted: 0, ...c };
}

@Component({
  selector: 'app-sg-elements',
  standalone: true,
  imports: [
    BadgeComponent, CopyFieldComponent, FieldRowComponent, IconComponent,
    MessageBannerComponent, EmptyStateComponent, LoadingSpinnerComponent,
    StatCardComponent, TableComponent, DividerComponent, EvalStatusBadgeComponent,
    MetricChartComponent, ToastComponent, ButtonComponent,
    CardGridComponent, RowComponent, RowListComponent, SkeletonComponent,
    LogoComponent, StarButtonComponent, StatusIconComponent, SegmentedBarComponent,
    AllowedCapabilitiesComponent,
    PermissionPickerComponent,
  ],
  // The demo star toggles locally instead of writing the viewer's real stars.
  providers: [MessageService, { provide: StarsService, useValue: { set: () => of(true) } }],
  templateUrl: './elements.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrls: ['./demo.scss', './elements.component.scss'],
})
export class ElementsComponent {
  readonly demoPermissions = [
    { id: 'viewProject', mutating: false },
    { id: 'createTask', mutating: true },
    { id: 'deleteProject', mutating: true },
  ];
  demoSelection: Record<string, boolean> = { viewProject: true };

  private messages = inject(MessageService);

  evalStatuses = [
    'Queued', 'Fetching', 'EvaluatingFlake', 'EvaluatingDerivation',
    'Building', 'Waiting', 'Completed', 'Failed', 'Aborted',
  ] as const;
  statusPhases: StatusPhase[] = ['queued', 'waiting', 'running', 'success', 'failure', 'aborted'];
  statusPhase = signal<StatusPhase>('queued');
  allowed: AllowedCapabilities = { enable_fetch: true, enable_eval: true, enable_build: false };
  progressBars: { caption: string; counts?: BuildStatusCounts; segments?: BarSegment[] }[] = [
    { caption: 'Build counts: completed, failed, building, queued', counts: counts({ completed: 12, failed: 2, building: 3, queued: 8 }) },
    { caption: 'Every build substituted from a cache', counts: counts({ substituted: 40 }) },
    { caption: 'No builds yet', counts: counts({}) },
    { caption: 'Download of a known size, 17.5 / 43.9 MiB', segments: byteSegments(18_400_000, 46_000_000) },
    { caption: 'Download of an unknown size, pulsing at full width', segments: byteSegments(2_100_000, null) },
    { caption: 'Finished download', segments: [{ tone: 'completed', pct: 100 }] },
    { caption: 'Failed download', segments: [{ tone: 'failed', pct: 100 }] },
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
