/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { DeviceRequest, RunnerJobStatus, RunnerUnmet, WaitingReason } from '@core/models';
import type { StatusPhase } from '@shared/evaluation';

export function runnerJobPhase(status: RunnerJobStatus): StatusPhase {
  switch (status) {
    case 'created':
    case 'queued': return 'queued';
    case 'starting':
    case 'running': return 'running';
    case 'completed': return 'success';
    case 'failed': return 'failure';
    case 'aborted': return 'aborted';
  }
}

export function isOpenRunnerJob(status: RunnerJobStatus): boolean {
  return runnerJobPhase(status) === 'queued' || runnerJobPhase(status) === 'running';
}

export function deviceLabel(device: DeviceRequest): string {
  return device.count === 1 ? device.name : `${device.name}:${device.count}`;
}

function unmetParts(unmet: RunnerUnmet): string[] {
  return [
    ...unmet.labels.map((label) => `label ${label}`),
    ...unmet.devices.map(deviceLabel),
    ...unmet.mounts.map((mount) => `mount ${mount}`),
    ...(unmet.nodes > 1 ? [`${unmet.nodes} nodes`] : []),
    ...(unmet.custom_image ? ['custom images'] : []),
  ];
}

export function waitingReasonText(reason: WaitingReason | null | undefined): string | null {
  if (!reason) return null;
  switch (reason.kind) {
    case 'eval_workers':
      return `waiting for a worker with ${reason.capability}`;
    case 'runner_workers': {
      const parts = unmetParts(reason.unmet);
      if (parts.length > 0) return `needs ${parts.join(', ')}`;
      const workers = reason.matching_workers;
      if (workers === 0) return 'waiting for a matching worker';
      return `waiting for free devices on ${workers} ${workers === 1 ? 'worker' : 'workers'}`;
    }
    default:
      return null;
  }
}
