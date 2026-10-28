/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { ExecJobStatus, WaitingReason } from '@core/models';
import type { StatusPhase } from '@shared/evaluation';
import { deviceLabel, guestSizeLabel, isOpenExecJob, execJobPhase, waitingReasonText } from './exec-job-status';

describe('execJobPhase', () => {
  it.each<[ExecJobStatus, StatusPhase]>([
    ['created', 'queued'],
    ['queued', 'queued'],
    ['starting', 'running'],
    ['running', 'running'],
    ['completed', 'success'],
    ['failed', 'failure'],
    ['aborted', 'aborted'],
  ])('maps %s to %s', (status, phase) => {
    expect(execJobPhase(status)).toBe(phase);
  });

  it('treats only completed, failed and aborted as finished', () => {
    expect(isOpenExecJob('starting')).toBe(true);
    expect(isOpenExecJob('aborted')).toBe(false);
  });
});

describe('waitingReasonText', () => {
  it('names the missing exec capability', () => {
    const reason: WaitingReason = { kind: 'eval_workers', capability: 'exec', connected_workers: 1 };
    expect(waitingReasonText(reason)).toBe('waiting for a worker with exec');
  });

  it('lists what no worker offers', () => {
    const reason: WaitingReason = {
      kind: 'exec_workers',
      unmet: { labels: ['3d-printer'], devices: [{ name: 'gpu:h100', count: 16 }], mounts: [], nodes: 0, custom_image: false },
      connected_workers: 1,
      matching_workers: 0,
    };
    expect(waitingReasonText(reason)).toBe('needs label 3d-printer, gpu:h100:16');
  });

  it('names a missing architecture', () => {
    const reason: WaitingReason = {
      kind: 'exec_workers',
      unmet: { architecture: 'aarch64-linux', labels: [], devices: [], mounts: [], nodes: 0, custom_image: false },
      connected_workers: 2,
      matching_workers: 0,
    };
    expect(waitingReasonText(reason)).toBe('needs architecture aarch64-linux');
  });

  it('names a secret no worker offers', () => {
    const reason: WaitingReason = {
      kind: 'exec_workers',
      unmet: { labels: [], devices: [], mounts: [], nodes: 0, custom_image: false, secrets: ['lab-key'] },
      connected_workers: 1,
      matching_workers: 0,
    };
    expect(waitingReasonText(reason)).toBe('needs worker secret lab-key');
  });

  it('names a worker that allows custom images', () => {
    const reason: WaitingReason = {
      kind: 'exec_workers',
      unmet: { labels: [], devices: [], mounts: [], nodes: 0, custom_image: true },
      connected_workers: 2,
      matching_workers: 0,
    };
    expect(waitingReasonText(reason)).toBe('needs a worker that allows custom images');
  });

  it('a nodes wait reason names the worker count', () => {
    const reason: WaitingReason = {
      kind: 'exec_workers',
      unmet: { labels: [], devices: [], mounts: [], nodes: 2, custom_image: false },
      connected_workers: 1,
      matching_workers: 1,
    };
    expect(waitingReasonText(reason)).toBe('needs 2 workers in one zone');
  });

  it('names the guest size no worker can hold', () => {
    const reason: WaitingReason = {
      kind: 'exec_workers',
      unmet: {
        labels: [], devices: [], mounts: [], nodes: 0, custom_image: false,
        guest: { cpus: 64, memory_mb: 0, disk_mb: 0 },
      },
      connected_workers: 1,
      matching_workers: 0,
    };
    expect(waitingReasonText(reason)).toBe('needs 64 CPUs');
  });

  it('counts the matching workers whose devices are busy', () => {
    const reason: WaitingReason = {
      kind: 'exec_workers',
      unmet: { labels: [], devices: [], mounts: [], nodes: 0, custom_image: false },
      connected_workers: 3,
      matching_workers: 2,
    };
    expect(waitingReasonText(reason)).toBe('waiting for free devices on 2 workers');
  });

  it('has no text without a reason', () => {
    expect(waitingReasonText(null)).toBeNull();
  });
});

describe('guestSizeLabel', () => {
  it('lists CPUs, memory and disk', () => {
    expect(guestSizeLabel({ cpus: 2, memory_mb: 500, disk_mb: 10240 })).toBe('2 CPUs · 500 MB memory · 10240 MB disk');
    expect(guestSizeLabel({ cpus: 1, memory_mb: 768, disk_mb: 256 })).toBe('1 CPU · 768 MB memory · 256 MB disk');
  });
});

describe('deviceLabel', () => {
  it('leaves out a count of one', () => {
    expect(deviceLabel({ name: 'kvm', count: 1 })).toBe('kvm');
    expect(deviceLabel({ name: 'gpu:h100', count: 8 })).toBe('gpu:h100:8');
  });
});
