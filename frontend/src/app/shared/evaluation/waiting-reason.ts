/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { UnmetRequirement, WaitingReason } from '@core/models';
import { unmetParts } from '@features/exec-jobs/exec-job-status';

export function waitingTitle(reason?: WaitingReason | null): string {
  switch (reason?.kind) {
    case 'eval_workers': return {
      network: 'Waiting for a Network Worker',
      secrets: 'Waiting for a Worker with Secrets',
      eval: 'Waiting for an Eval Worker',
      exec: 'Waiting for an Exec Worker',
    }[reason.capability];
    case 'approval': return 'Awaiting Approval';
    case 'no_cache': return 'No Cache Configured';
    case 'cache_storage_full': return 'Cache Storage Full';
    case 'draining': return 'Instance Draining';
    case 'graph_stuck': return 'Recovering Build Graph';
    case 'upstream_caches': return 'Waiting for Upstream Caches';
    default: return 'Waiting for Workers';
  }
}

export function formatWaitingReason(reason: WaitingReason): string {
  switch (reason.kind) {
    case 'workers': {
      if (reason.connected_workers === 0) {
        return 'No workers are connected. The evaluation requires:';
      }
      const archList = reason.available_architectures.length > 0
        ? reason.available_architectures.join(', ')
        : 'none';
      const workerWord = reason.connected_workers === 1 ? 'worker' : 'workers';
      return `${reason.connected_workers} connected ${workerWord} (${archList}) cannot satisfy:`;
    }
    case 'eval_workers': {
      const action = {
        network: 'fetch the flake sources',
        secrets: 'fetch the flake sources with the project SSH key',
        eval: 'run the evaluation',
        exec: 'run exec jobs',
      }[reason.capability];
      const workerWord = reason.connected_workers === 1 ? 'worker is' : 'workers are';
      return reason.connected_workers === 0
        ? `No workers are connected to ${action}.`
        : `${reason.connected_workers} ${workerWord} connected, but none can ${action}.`;
    }
    case 'approval':
      return `Awaiting maintainer approval for pull request #${reason.pr_number} by ${reason.pr_author}.`;
    case 'no_cache':
      return 'No cache is configured for this project. Configure a cache before this evaluation can run.';
    case 'cache_storage_full':
      return 'Every writable cache for this project is full. Free space or raise the limit before this evaluation can run.';
    case 'draining':
      return 'The instance is draining. This evaluation resumes when draining is disabled or the server restarts.';
    case 'graph_stuck': {
      const buildWord = reason.pending_shared_builds === 1 ? 'build is' : 'builds are';
      return `Workers are available, but ${reason.pending_shared_builds} ${buildWord} blocked on dependencies. Recovering automatically.`;
    }
    case 'upstream_caches': {
      const buildWord = reason.pending_shared_builds === 1 ? 'build waits' : 'builds wait';
      const failing = reason.failing_upstream_caches.length > 0
        ? ` Not answering: ${reason.failing_upstream_caches.join(', ')}.`
        : '';
      return `${reason.pending_shared_builds} ${buildWord} for an answer from the upstream caches.${failing}`;
    }
    case 'exec_workers': {
      const missing = unmetParts(reason.unmet);
      if (missing.length > 0) return `The exec job needs ${missing.join(', ')}.`;
      const workerWord = reason.matching_workers === 1 ? 'worker' : 'workers';
      return reason.matching_workers > 0
        ? `Waiting for free devices on ${reason.matching_workers} ${workerWord}.`
        : 'No connected worker offers what the exec job needs.';
    }
    case 'manual_start':
      return 'The exec job waits for a manual start.';
  }
}

export function unmetRequirementLabel(req: UnmetRequirement): string {
  const features = req.required_features.length
    ? ` with feature${req.required_features.length === 1 ? '' : 's'} ${req.required_features.join(', ')}`
    : '';
  return `${req.architecture}${features} - ${req.build_count} ${req.build_count === 1 ? 'build' : 'builds'}`;
}
