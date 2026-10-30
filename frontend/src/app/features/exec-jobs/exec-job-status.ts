/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { DeviceRequest, ExecJobStatus, ExecJobSummary, ExecUnmet, GuestSize, WaitingReason } from '@core/models';
import type { StatusPhase } from '@shared/evaluation';
import type { MenuItem } from '@gradient/ui/ui';
import { boardJobLink } from '@features/board/job-kind';

export function execJobPhase(status: ExecJobStatus): StatusPhase {
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

export function isOpenExecJob(status: ExecJobStatus): boolean {
  return execJobPhase(status) === 'queued' || execJobPhase(status) === 'running';
}

export function hasExecTerminal(status: ExecJobStatus): boolean {
  return status === 'starting' || status === 'running';
}

export interface ExecJobMenuActions {
  terminal: Pick<MenuItem, 'routerLink' | 'queryParams' | 'command'>;
  abort: () => void;
}

export function execJobMenu(
  { status, dispatched_job: job, cluster_job: cluster }: Pick<ExecJobSummary, 'status' | 'dispatched_job' | 'cluster_job'>,
  canTrigger: boolean,
  actions: ExecJobMenuActions,
): MenuItem[] {
  const link = boardJobLink(job, cluster);
  return [
    { label: 'Show job', icon: 'work', disabled: !link, routerLink: link },
    ...(hasExecTerminal(status) ? [{ label: 'Terminal', icon: 'terminal', ...actions.terminal }] : []),
    ...(isOpenExecJob(status) && canTrigger ? [{ label: 'Abort', icon: 'stop_circle', command: actions.abort }] : []),
  ];
}

export function deviceLabel(device: DeviceRequest): string {
  return device.count === 1 ? device.name : `${device.name}:${device.count}`;
}

function cpusLabel(cpus: number): string {
  return cpus === 1 ? '1 CPU' : `${cpus} CPUs`;
}

function guestSizeParts(size: GuestSize): string[] {
  return [
    ...(size.cpus > 0 ? [cpusLabel(size.cpus)] : []),
    ...(size.memory_mb > 0 ? [`${size.memory_mb} MB memory`] : []),
    ...(size.disk_mb > 0 ? [`${size.disk_mb} MB disk`] : []),
  ];
}

export function guestSizeLabel(size: GuestSize): string {
  return guestSizeParts(size).join(' · ');
}

export function unmetParts(unmet: ExecUnmet): string[] {
  return [
    ...(unmet.architecture ? [`architecture ${unmet.architecture}`] : []),
    ...unmet.labels.map((label) => `label ${label}`),
    ...unmet.devices.map(deviceLabel),
    ...(unmet.guest ? guestSizeParts(unmet.guest) : []),
    ...unmet.mounts.map((mount) => `mount ${mount}`),
    ...(unmet.nodes > 1 ? [`${unmet.nodes} workers in one zone`] : []),
    ...(unmet.custom_image ? ['a worker that allows custom images'] : []),
    ...(unmet.secrets ?? []).map((name) => `worker secret ${name}`),
  ];
}

export function waitingReasonText(reason: WaitingReason | null | undefined): string | null {
  if (!reason) return null;
  switch (reason.kind) {
    case 'eval_workers':
      return `waiting for a worker with ${reason.capability}`;
    case 'exec_workers': {
      const parts = unmetParts(reason.unmet);
      if (parts.length > 0) return `needs ${parts.join(', ')}`;
      const workers = reason.matching_workers;
      if (workers === 0) return 'waiting for a matching worker';
      return `waiting for free devices on ${workers} ${workers === 1 ? 'worker' : 'workers'}`;
    }
    case 'manual_start':
      return 'waits for a manual start';
    default:
      return null;
  }
}
