/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { UrlTree } from '@angular/router';
import { Permission, UnmetRequirement, WaitingReason } from '@core/models';
import { unmetParts } from '@features/exec-jobs/exec-job-status';
import { links } from '@shared/routing';

export interface WaitingAccess {
  manageWorkers: boolean;
  manageSubscriptions: boolean;
  trigger: boolean;
  superuser: boolean;
}

export function waitingAccessFrom(permissions: readonly Permission[], trigger: boolean, superuser: boolean): WaitingAccess {
  return {
    manageWorkers: permissions.includes('manageWorkers'),
    manageSubscriptions: permissions.includes('manageSubscriptions'),
    trigger,
    superuser,
  };
}

export type WaitingAction =
  | { kind: 'link'; label: string; icon: string; link: UrlTree }
  | { kind: 'start'; label: string; icon: string };

export interface WaitingExplanation {
  title: string;
  detail: string;
  requirements: string[];
  action: WaitingAction | null;
  hint: string | null;
}

const ASK_FOR_WORKER = 'Ask a project admin to register a worker that can meet the requirement.';

function workerAction(project: string): WaitingAction {
  return { kind: 'link', label: 'Register worker', icon: 'construction', link: links.projectPage(project, 'workers') };
}

function cachesAction(project: string, label: string): WaitingAction {
  return { kind: 'link', label, icon: 'database', link: links.projectPage(project, 'caches') };
}

function plural(count: number, word: string): string {
  return `${count} ${word}${count === 1 ? '' : 's'}`;
}

export function unmetRequirementLabel(req: UnmetRequirement): string {
  const features = req.required_features.length
    ? ` with feature${req.required_features.length === 1 ? '' : 's'} ${req.required_features.join(', ')}`
    : '';
  return `${req.architecture}${features} - ${plural(req.build_count, 'build')}`;
}

// All waiting reasons map to an explanation and a next action for the viewer in one place. The task, log, running-jobs and board pages all use that mapping.
export function explainWaiting(reason: WaitingReason | null | undefined, project: string, access: WaitingAccess): WaitingExplanation {
  const step = (allowed: boolean, action: WaitingAction, hint: string) =>
    allowed ? { action, hint: null } : { action: null, hint };
  const forWorkerAdmin = (action: WaitingAction, hint: string) => step(access.manageWorkers, action, hint);
  const forSubscriptionAdmin = (action: WaitingAction, hint: string) => step(access.manageSubscriptions, action, hint);

  switch (reason?.kind) {
    case undefined:
      return {
        title: 'Waiting for a worker',
        detail: 'Currently, no build worker is available. The evaluation can start again after a worker connection.',
        requirements: [],
        ...forWorkerAdmin(workerAction(project), ASK_FOR_WORKER),
      };
    case 'workers': {
      const architectures = reason.available_architectures.join(', ') || 'none';
      return {
        title: 'Waiting for a worker',
        detail: reason.connected_workers === 0
          ? 'No worker is connected.'
          : `${plural(reason.connected_workers, 'connected worker')} (${architectures}) can't build this evaluation.`,
        requirements: reason.unmet.map(unmetRequirementLabel),
        ...forWorkerAdmin(workerAction(project), ASK_FOR_WORKER),
      };
    }
    case 'eval_workers': {
      const work = {
        network: 'fetch the flake sources',
        secrets: 'fetch the flake sources with the project SSH key',
        eval: 'run the evaluation',
        exec: 'run exec jobs',
      }[reason.capability];
      return {
        title: {
          network: 'Waiting for network worker',
          secrets: 'Waiting for worker with secrets',
          eval: 'Waiting for evaluation worker',
          exec: 'Waiting for exec worker',
        }[reason.capability],
        detail: reason.connected_workers === 0
          ? `No workers are connected to ${work}.`
          : `${reason.connected_workers} ${reason.connected_workers === 1 ? 'worker is' : 'workers are'} connected, but none can ${work}.`,
        requirements: [],
        ...forWorkerAdmin(workerAction(project), ASK_FOR_WORKER),
      };
    }
    case 'approval':
      return {
        title: 'Waiting for approval',
        detail: `Awaiting maintainer approval for pull request #${reason.pr_number} by ${reason.pr_author}.`,
        requirements: [],
        action: null,
        hint: 'A maintainer can approve the pull request with the "Approve and run" check or the /gradient approve comment.',
      };
    case 'no_cache':
      return {
        title: 'Cache not configured',
        detail: 'A cache is necessary to store build results. Subscribe this project to a cache before the evaluation can run.',
        requirements: [],
        ...forSubscriptionAdmin(cachesAction(project, 'Configure cache'), 'Ask a project admin to subscribe this project to a cache.'),
      };
    case 'cache_storage_full':
      return {
        title: 'Cache storage full',
        detail: 'Every writable cache for this project is full. Free space or raise the limit before this evaluation can run.',
        requirements: [],
        ...forSubscriptionAdmin(cachesAction(project, 'Open the cache subscription'),
          "Ask a project admin to free space in this project's cache or raise the limit."),
      };
    case 'draining':
      return {
        title: 'Instance draining',
        detail: 'The instance is draining. This evaluation resumes when draining is disabled or the server restarts.',
        requirements: [],
        ...step(access.superuser, { kind: 'link', label: 'View system health', icon: 'monitor_heart', link: links.board('health') },
          'Ask an instance admin to stop draining.'),
      };
    case 'graph_stuck':
      return {
        title: 'Restoring build graph',
        detail: `Workers are available, but ${reason.pending_shared_builds} ${reason.pending_shared_builds === 1 ? 'build is' : 'builds are'} blocked on dependencies. Recovering automatically.`,
        requirements: [],
        action: null,
        hint: null,
      };
    case 'upstream_caches': {
      const failing = reason.failing_upstream_caches.length > 0
        ? ` Not answering: ${reason.failing_upstream_caches.join(', ')}.`
        : '';
      return {
        title: 'Waiting for upstream cache',
        detail: `${reason.pending_shared_builds} ${reason.pending_shared_builds === 1 ? 'build waits' : 'builds wait'} for an answer from the upstream caches.${failing}`,
        requirements: [],
        ...forSubscriptionAdmin(cachesAction(project, 'Open the cache subscription'),
          "Ask a cache admin to check the upstream caches for this project's cache."),
      };
    }
    case 'exec_workers': {
      const missing = unmetParts(reason.unmet);
      return {
        title: 'Waiting for exec worker',
        detail: missing.length === 0 && reason.matching_workers > 0
          ? `Waiting for free devices on ${plural(reason.matching_workers, 'worker')}.`
          : 'No connected worker offers what the exec job needs.',
        requirements: missing,
        ...forWorkerAdmin(workerAction(project), ASK_FOR_WORKER),
      };
    }
    case 'manual_start':
      return {
        title: 'Waiting for manual start',
        detail: 'A person can start an exec job manually.',
        requirements: [],
        ...step(access.trigger, { kind: 'start', label: 'Run', icon: 'play_arrow' },
          'Ask a project member with permission to start runs to start the job.'),
      };
  }
}
