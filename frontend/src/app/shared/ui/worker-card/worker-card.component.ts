/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, computed, input } from '@angular/core';
import type { ProjectWorkerEntry, TeamWorkerEntry } from '@core/models';
import { BadgeComponent, IconComponent, PopoverComponent } from '@gradient/ui/ui';
import { AllowedCapabilitiesComponent } from '../allowed-capabilities/allowed-capabilities.component';
import { WorkerDrainingBadgeComponent } from '../worker-drain/worker-drain';

type SharedWorkerEntryPropertyNames = keyof ProjectWorkerEntry & keyof TeamWorkerEntry;

export type WorkerCardWorkerInput = Pick<ProjectWorkerEntry, SharedWorkerEntryPropertyNames> &
  Partial<Pick<TeamWorkerEntry, 'requested_by_admin'>>;

@Component({
  selector: 'app-worker-card-list',
  standalone: true,
  template: '<ng-content></ng-content>',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './worker-card-list.component.scss',
})
export class WorkerCardGridComponent {}

@Component({
  selector: 'app-worker-card',
  standalone: true,
  imports: [AllowedCapabilitiesComponent, BadgeComponent, IconComponent, PopoverComponent, WorkerDrainingBadgeComponent],
  templateUrl: './worker-card.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './worker-card.component.scss',
  host: { '[class.is-offline]': '!worker().connected' },
})
export class WorkerCardComponent {
  worker = input.required<WorkerCardWorkerInput>();
  showWorkerLastConnectionErrorReason = input(false, { transform: booleanAttribute });

  protected readonly liveReportOfActiveWorker = computed(() => {
    const worker = this.worker();
    return worker.active ? worker.live ?? null : null;
  });

  protected readonly workerLoadText = computed(() => {
    const live = this.liveReportOfActiveWorker();
    return live ? `${live.assigned_job_count} / ${live.max_concurrent_builds}` : '–';
  });

  protected readonly jobCountStatusText = computed(() => {
    const worker = this.worker();
    if (this.liveReportOfActiveWorker()) return 'Running jobs';
    if (!worker.active) return '';
    if (worker.connected) return 'No jobs for this project';
    if (worker.last_error && this.showWorkerLastConnectionErrorReason()) return `last error: ${worker.last_error.reason}`;
    return '';
  });

  protected readonly jobCountStatusTitle = computed(() => {
    const worker = this.worker();
    if (worker.active && worker.connected && !this.liveReportOfActiveWorker()) {
      return 'Reconnect the worker with the project token to run jobs for this project.';
    }
    return this.jobCountStatusText();
  });
}
