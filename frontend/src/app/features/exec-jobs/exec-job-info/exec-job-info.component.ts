/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { TableComponent } from '@gradient/ui/ui';
import { formatDuration } from '@shared/text';
import type { ExecJobResponse } from '@core/models';
import { deviceLabel, guestSizeLabel } from '../exec-job-status';

interface RequestedField {
  label: string;
  values: string[];
}

function field(label: string, values: (string | null | undefined)[]): RequestedField {
  return { label, values: values.filter((value): value is string => !!value) };
}

function seconds(secs: number | null | undefined): string | null {
  return secs == null ? null : formatDuration(secs * 1000);
}

function yesNo(value: boolean): string {
  return value ? 'Yes' : 'No';
}

function requestedFields(job: ExecJobResponse): RequestedField[] {
  return [
    field('Architecture', [job.architecture]),
    field('Guest', [guestSizeLabel(job.guest)]),
    field('Nodes', [String(job.nodes)]),
    field('Labels', job.labels),
    field('Devices', job.devices.map(deviceLabel)),
    field('Mounts', job.mounts),
    field('Secrets', job.secrets),
    field('Network', [yesNo(job.network)]),
    field('SSH', [yesNo(job.ssh)]),
    field('Ports', job.ports.map(String)),
    field('Image', [job.image]),
    field('Timeout', [seconds(job.timeout_secs)]),
    field('Time without output', [seconds(job.max_silent_secs)]),
    field('Attempt', [`${job.attempts.length} of ${job.retries + 1}`]),
  ].filter((requested) => requested.values.length > 0);
}

@Component({
  selector: 'app-exec-job-info',
  standalone: true,
  imports: [TableComponent],
  templateUrl: './exec-job-info.component.html',
  styleUrl: './exec-job-info.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ExecJobInfoComponent {
  job = input.required<ExecJobResponse>();

  fields = computed(() => requestedFields(this.job()));
}
