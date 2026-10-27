/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { RunnerJobStatus, WaitingReason } from '@core/models';
import type { StatusPhase } from '@shared/evaluation';
import { deviceLabel, isOpenRunnerJob, runnerJobPhase, waitingReasonText } from './runner-job-status';

describe('runnerJobPhase', () => {
  it.each<[RunnerJobStatus, StatusPhase]>([
    ['created', 'queued'],
    ['queued', 'queued'],
    ['starting', 'running'],
    ['running', 'running'],
    ['completed', 'success'],
    ['failed', 'failure'],
    ['aborted', 'aborted'],
  ])('maps %s to %s', (status, phase) => {
    expect(runnerJobPhase(status)).toBe(phase);
  });

  it('treats only completed, failed and aborted as finished', () => {
    expect(isOpenRunnerJob('starting')).toBe(true);
    expect(isOpenRunnerJob('aborted')).toBe(false);
  });
});

describe('waitingReasonText', () => {
  it('names the missing exec capability', () => {
    const reason: WaitingReason = { kind: 'eval_workers', capability: 'exec', connected_workers: 1 };
    expect(waitingReasonText(reason)).toBe('waiting for a worker with exec');
  });

  it('lists what no worker offers', () => {
    const reason: WaitingReason = {
      kind: 'runner_workers',
      unmet: { labels: ['3d-printer'], devices: [{ name: 'gpu:h100', count: 16 }], mounts: [], nodes: 0, custom_image: false },
      connected_workers: 1,
      matching_workers: 0,
    };
    expect(waitingReasonText(reason)).toBe('needs label 3d-printer, gpu:h100:16');
  });

  it('counts the matching workers whose devices are busy', () => {
    const reason: WaitingReason = {
      kind: 'runner_workers',
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

describe('deviceLabel', () => {
  it('leaves out a count of one', () => {
    expect(deviceLabel({ name: 'kvm', count: 1 })).toBe('kvm');
    expect(deviceLabel({ name: 'gpu:h100', count: 8 })).toBe('gpu:h100:8');
  });
});
