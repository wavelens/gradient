/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { EMPTY } from 'rxjs';
import {
  abortExecJobConfirm, deviceLabel, getExecJobStatusBadgeLabel, guestSizeLabel, isOpenExecJob,
} from './exec-job-status';

describe('abortExecJobConfirm', () => {
  it('names the consequence for the program', () => {
    expect(abortExecJobConfirm('deploy', () => EMPTY).consequence).toBe('The program stops and its VM is removed.');
  });
});

describe('isOpenExecJob', () => {
  it('treats only completed, failed and aborted as finished', () => {
    expect(isOpenExecJob('created')).toBe(true);
    expect(isOpenExecJob('starting')).toBe(true);
    expect(isOpenExecJob('aborted')).toBe(false);
  });
});

describe('getExecJobStatusBadgeLabel', () => {
  it('names a job that waits for a manual start', () => {
    expect(getExecJobStatusBadgeLabel({ status: 'created', waiting_reason: { kind: 'manual_start' } })).toBe('Manual');
  });

  it('names the status of a job that waits for its builds', () => {
    expect(getExecJobStatusBadgeLabel({ status: 'created', waiting_reason: null })).toBe('Waiting for builds');
  });

  it('names why a failed job ended', () => {
    expect(getExecJobStatusBadgeLabel({ status: 'failed', waiting_reason: null, end_reason: 'timeout' })).toBe('Timed out');
    expect(getExecJobStatusBadgeLabel({ status: 'failed', waiting_reason: null, end_reason: null })).toBe('Failed');
  });
});

describe('guestSizeLabel', () => {
  it('lists CPUs, memory and disk', () => {
    expect(guestSizeLabel({ cpus: 2, memory_mb: 500, disk_mb: 2048 })).toBe('2 CPUs · 500 MB memory · 2048 MB disk');
    expect(guestSizeLabel({ cpus: 1, memory_mb: 768, disk_mb: 256 })).toBe('1 CPU · 768 MB memory · 256 MB disk');
  });
});

describe('deviceLabel', () => {
  it('leaves out a count of one', () => {
    expect(deviceLabel({ name: 'kvm', count: 1 })).toBe('kvm');
    expect(deviceLabel({ name: 'gpu:h100', count: 8 })).toBe('gpu:h100:8');
  });
});
