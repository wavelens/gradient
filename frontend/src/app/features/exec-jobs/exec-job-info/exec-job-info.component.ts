/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { BadgeComponent, TableComponent } from '@gradient/ui/ui';
import { StatusIconComponent } from '@shared/ui';
import { formatEvaluationDuration } from '@shared/evaluation';
import type { ExecJobResponse } from '@core/models';
import { deviceLabel, execJobPhase, guestSizeLabel, unmetParts, waitingReasonText } from '../exec-job-status';

interface RequestedField {
  label: string;
  values: string[];
  unset: string;
}

function field(label: string, values: (string | null | undefined)[], unset = 'None'): RequestedField {
  return { label, values: values.filter((value): value is string => !!value), unset };
}

function seconds(secs: number | null | undefined): string | null {
  return secs == null ? null : formatEvaluationDuration(secs * 1000);
}

function yesNo(value: boolean): string {
  return value ? 'Yes' : 'No';
}

function requestedFields(job: ExecJobResponse): RequestedField[] {
  return [
    field('Architecture', [job.architecture], 'Any'),
    field('Guest', [guestSizeLabel(job.guest)], 'Worker default'),
    field('Nodes', [String(job.nodes)]),
    field('Labels', job.labels),
    field('Devices', job.devices.map(deviceLabel)),
    field('Mounts', job.mounts),
    field('Secrets', job.secrets),
    field('Network', [yesNo(job.network)]),
    field('SSH', [yesNo(job.ssh)]),
    field('Ports', job.ports.map(String)),
    field('Image', [job.image], 'Default'),
    field('Timeout', [seconds(job.timeout_secs)], 'Worker limit'),
    field('Time without output', [seconds(job.max_silent_secs)], 'Worker limit'),
    field('Retries', [String(job.retries)]),
  ];
}

@Component({
  selector: 'app-exec-job-info',
  standalone: true,
  imports: [BadgeComponent, TableComponent, StatusIconComponent],
  templateUrl: './exec-job-info.component.html',
  styleUrl: './exec-job-info.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ExecJobInfoComponent {
  job = input.required<ExecJobResponse>();

  fields = computed(() => requestedFields(this.job()));
  waiting = computed(() => waitingReasonText(this.job().waiting_reason));
  missing = computed(() => {
    const reason = this.job().waiting_reason;
    return reason?.kind === 'exec_workers' ? unmetParts(reason.unmet) : [];
  });

  protected readonly phase = execJobPhase;
}
