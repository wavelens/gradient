/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ActivatedRouteSnapshot, PRIMARY_OUTLET, Params, UrlSegment, UrlSegmentGroup, UrlTree } from '@angular/router';

export type ProjectPage = 'settings' | 'connect' | 'members' | 'workers' | 'integrations' | 'secrets' | 'webhooks' | 'caches';
export type TaskPage = 'settings' | 'repository' | 'triggers' | 'actions' | 'flake-inputs' | 'metrics';
export type CachePage = 'settings' | 'upstream-caches' | 'nars' | 'members-roles' | 'subscriptions' | 'webhooks';
export type TeamPage = 'settings' | 'webhooks';
export type SettingsPage = 'profile' | 'keys' | 'ssh-keys' | 'sessions' | 'invites';
export type BoardTab =
  | 'overview' | 'live' | 'scheduler' | 'throughput' | 'durations' | 'workers' | 'cache'
  | 'network' | 'health' | 'storage' | 'expensive';
export type AdminPage = 'webhooks' | 'github-app';
export type ClosureKind = 'build' | 'eval';
export type LoginReason = 'expired' | 'deleted';

export interface BuildOrigin {
  task?: string | null;
  evaluation?: string | null;
}

type Query = Record<string, string | number | null | undefined>;

function url(path: readonly string[], query: Query = {}): UrlTree {
  const queryParams: Params = Object.fromEntries(
    Object.entries(query).filter(([, value]) => value !== null && value !== undefined && value !== ''),
  );
  const children: Record<string, UrlSegmentGroup> = {};
  if (path.length) children[PRIMARY_OUTLET] = new UrlSegmentGroup(path.map((segment) => new UrlSegment(segment, {})), {});
  return new UrlTree(new UrlSegmentGroup([], children), queryParams);
}

function origin(from: BuildOrigin): Query {
  return { task: from.task, eval: from.evaluation };
}

export function safeInAppPathOrNull(path: string | null | undefined): string | null {
  return path?.startsWith('/') && !path.startsWith('//') && !path.includes('\\') ? path : null;
}

export function readInAppNextPathFromRouteSnapshot(route: ActivatedRouteSnapshot): string | null {
  return safeInAppPathOrNull(route.queryParamMap.get('next'));
}

export function getInAppReturnPathIfNotErrorPage(from: string | null | undefined): string | null {
  const path = safeInAppPathOrNull(from);
  return path && !path.startsWith('/error/') ? path : null;
}

// All navigation targets in the app originate in this file. The tests in links.spec.ts check each one against app.routes.ts.
export const links = {
  dashboard: () => url([]),
  login: (next?: string | null, reason?: LoginReason) => url(['account', 'login'], { next, reason }),
  loginPageAfterSingleSignOnError: (code: string) => url(['account', 'login'], { login_error: code }),
  loginAfterRegister: (username: string, next?: string | null) => url(['account', 'login'], { registered: username, next }),
  register: (next?: string | null) => url(['account', 'register'], { next }),

  projects: () => url(['projects']),
  project: (project: string) => url(['project', project]),
  projectPage: (project: string, page: ProjectPage) => url(['project', project, page]),
  subscribeToCache: (project: string, cache: string) => url(['project', project, 'caches'], { subscribe: cache }),
  integrationForTrigger: (project: string, task: string, trigger: string) =>
    url(['project', project, 'integrations'], { task, trigger }),
  workerMetrics: (project: string, worker: string) => url(['project', project, 'workers', worker, 'metrics']),

  task: (project: string, task: string, evaluation?: string | null) =>
    url(['project', project, 'task', task], { eval: evaluation }),
  taskPage: (project: string, task: string, page: TaskPage) => url(['project', project, 'task', task, page]),
  editTrigger: (project: string, task: string, trigger: string, integration?: string | null) =>
    url(['project', project, 'task', task, 'triggers'], { edit: trigger, integration }),
  entryPointMetrics: (project: string, task: string, entryPoint: string) =>
    url(['project', project, 'task', task, 'entry-point-metrics'], { eval: entryPoint }),
  execJob: (project: string, task: string, job: string, options: { node?: number; terminal?: boolean } = {}) =>
    url(['project', project, 'task', task, 'exec', job], {
      node: options.node || null,
      view: options.terminal ? 'terminal' : null,
    }),

  evaluationLog: (project: string, evaluation: string, options: { build?: string | null; package?: string | null; messages?: boolean } = {}) =>
    url(['project', project, 'log', evaluation], { build: options.build, package: options.package, eval: options.messages ? 1 : null }),
  evaluationSummary: (project: string, evaluation: string) => url(['project', project, 'summary', evaluation]),
  artefacts: (project: string, build: string, from: BuildOrigin = {}) =>
    url(['project', project, 'artefacts', build], origin(from)),
  dependencyGraph: (project: string, build: string, from: BuildOrigin = {}) =>
    url(['project', project, 'graph', build], origin(from)),
  closure: (project: string, kind: ClosureKind, id: string, from: BuildOrigin = {}) =>
    url(['project', project, 'closure', kind, id], origin(from)),

  caches: () => url(['caches']),
  cache: (cache: string) => url(['caches', cache]),
  cachePage: (cache: string, page: CachePage) => url(['caches', cache, page]),

  teams: () => url(['teams']),
  team: (team: string) => url(['team', team]),
  teamPage: (team: string, page: TeamPage) => url(['team', team, page]),

  board: (tab: BoardTab = 'overview') => url(['board', tab]),
  boardJob: (job: string) => url(['board', 'jobs', job]),
  boardCluster: (cluster: string) => url(['board', 'clusters', cluster]),
  boardJobOrCluster: (job?: string | null, cluster?: string | null): UrlTree | undefined => {
    if (cluster) return url(['board', 'clusters', cluster]);
    return job ? url(['board', 'jobs', job]) : undefined;
  },
  boardPendingJob: (pending: { cluster_job?: string | null; exec_job?: { id: string } | null; build_id?: string | null; evaluation_id: string }) =>
    pending.cluster_job
      ? url(['board', 'clusters', pending.cluster_job])
      : url(['board', 'pending', pending.exec_job?.id ?? pending.build_id ?? pending.evaluation_id]),
  boardExecJob: (exec: { project: string; task: string; id: string; node_index: number }) =>
    url(['project', exec.project, 'task', exec.task, 'exec', exec.id], { node: exec.node_index || null }),

  settings: (page: SettingsPage = 'profile') => url(['settings', page]),
  admin: (page: AdminPage) => url(['admin', page]),
} as const;

export function readBuildOrigin(query: { get(name: string): string | null }): BuildOrigin {
  return { task: query.get('task'), evaluation: query.get('eval') ?? query.get('evalId') };
}
