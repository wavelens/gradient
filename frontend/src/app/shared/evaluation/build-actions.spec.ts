/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  canAbortBuild, canPrioritizeBuild, canRetryBuild, getArtefactsMenuItemDisabledReason, getClosureMenuItemDisabledReason,
  getDownloadLogMenuItemDisabledReason, showJobMenuItemDisabledReason,
} from './build-actions';

describe('build actions', () => {
  it('prioritizes only a pending build nobody prioritized yet', () => {
    expect(canPrioritizeBuild({ status: 'Queued', prioritized: false })).toBe(true);
    expect(canPrioritizeBuild({ status: 'Queued', prioritized: true })).toBe(false);
    expect(canPrioritizeBuild({ status: 'Completed', prioritized: false })).toBe(false);
  });

  it('aborts only a pending build of a running evaluation', () => {
    expect(canAbortBuild('Building', 'Building')).toBe(true);
    expect(canAbortBuild('Building', 'Created')).toBe(true);
    expect(canAbortBuild('Building', 'FailedPermanent')).toBe(false);
    expect(canAbortBuild('Aborted', 'Queued')).toBe(false);
  });

  it('retries a failed build inside a running evaluation', () => {
    const running = { status: 'Building', replaced: false } as const;
    expect(canRetryBuild(running, 'FailedPermanent')).toBe(true);
    expect(canRetryBuild(running, 'DependencyFailed')).toBe(true);
    expect(canRetryBuild(running, 'Aborted')).toBe(true);
    expect(canRetryBuild(running, 'Building')).toBe(false);
    expect(canRetryBuild(running, 'Completed')).toBe(false);
  });

  it('retries a failed build of a finished evaluation only while no newer evaluation replaced it', () => {
    expect(canRetryBuild({ status: 'Failed', replaced: false }, 'FailedTimeout')).toBe(true);
    expect(canRetryBuild({ status: 'Aborted', replaced: false }, 'Aborted')).toBe(true);
    expect(canRetryBuild({ status: 'Failed', replaced: true }, 'FailedTimeout')).toBe(false);
    expect(canRetryBuild({ status: 'Completed', replaced: false }, 'Aborted')).toBe(false);
  });

  it('gives a reason for every build menu item it disables and none for a usable one', () => {
    expect(getArtefactsMenuItemDisabledReason('Building', true)).toBe('Artefacts are available after a successful build.');
    expect(getArtefactsMenuItemDisabledReason('Completed', false)).toBe('No stored artefacts for this build.');
    expect(getArtefactsMenuItemDisabledReason('Substituted', true)).toBeNull();
    expect(getClosureMenuItemDisabledReason('Completed')).toBeNull();
    expect(getDownloadLogMenuItemDisabledReason('FailedPermanent')).toBeNull();
    expect(showJobMenuItemDisabledReason(null, true, 'evaluation')).toBe('No job for this evaluation on the job board.');
    expect(showJobMenuItemDisabledReason('job-1', true, 'build')).toBeNull();
  });
});
