/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ActivatedRouteSnapshot, Route, UrlTree, convertToParamMap } from '@angular/router';
import { routes } from '../../app.routes';
import { links,readBuildOrigin, readInAppNextPathFromRouteSnapshot } from './links';

function matches(config: readonly Route[], segments: readonly string[]): boolean {
  return config.some((route) => {
    if (route.path === undefined || route.path === '**') return false;
    const pattern = route.path === '' ? [] : route.path.split('/');
    if (pattern.length > segments.length) return false;
    const fits = pattern.every((part, i) => part.startsWith(':') || part === segments[i]);
    if (!fits) return false;
    const rest = segments.slice(pattern.length);
    if (route.children) return matches(route.children, rest);
    return rest.length === 0;
  });
}

function path(tree: UrlTree): string[] {
  return tree.root.children['primary']?.segments.map((segment) => segment.path) ?? [];
}

const every: UrlTree[] = [
  links.dashboard(),
  links.login('/projects'),
  links.register(),
  links.projects(),
  links.project('p'),
  ...(['settings', 'members', 'workers', 'integrations', 'secrets', 'webhooks', 'caches'] as const)
    .map((page) => links.projectPage('p', page)),
  links.workerMetrics('p', 'w'),
  links.subscribeToCache('p', 'c'),
  links.integrationForTrigger('p', 't', 'trigger'),
  links.editTrigger('p', 't', 'trigger', 'integration'),
  links.task('p', 't', 'e'),
  ...(['settings', 'triggers', 'actions', 'flake-inputs', 'metrics'] as const).map((page) => links.taskPage('p', 't', page)),
  links.entryPointMetrics('p', 't', 'packages.x86_64-linux.hello'),
  links.execJob('p', 't', 'j', { node: 1, terminal: true }),
  links.evaluationLog('p', 'e', { build: 'b' }),
  links.evaluationSummary('p', 'e'),
  links.artefacts('p', 'b', { task: 't', evaluation: 'e' }),
  links.dependencyGraph('p', 'b'),
  links.closure('p', 'build', 'b'),
  links.caches(),
  links.cache('c'),
  ...(['settings', 'upstream-caches', 'nars', 'members-roles', 'subscriptions', 'webhooks'] as const)
    .map((page) => links.cachePage('c', page)),
  links.teams(),
  links.team('t'),
  links.teamPage('t', 'settings'),
  links.teamPage('t', 'webhooks'),
  ...(['overview', 'live', 'scheduler', 'throughput', 'durations', 'workers', 'cache', 'network', 'health', 'storage',
    'expensive'] as const).map((tab) => links.board(tab)),
  links.boardJob('j'),
  links.boardCluster('c'),
  links.boardPendingJob({ evaluation_id: 'e', build_id: 'b' }),
  links.boardExecJob({ project: 'p', task: 't', id: 'x', node_index: 0 }),
  ...(['profile', 'keys', 'ssh-keys', 'sessions', 'invites'] as const).map((page) => links.settings(page)),
  links.admin('webhooks'),
  links.admin('github-app'),
];

describe('links', () => {
  it('points every link at a route that exists', () => {
    const missing = every.map(String).filter((_, i) => !matches(routes, path(every[i])));
    expect(missing).toEqual([]);
  });

  it('opens a board exec job on its page under the task', () => {
    expect(String(links.boardExecJob({ project: 'infra', task: 'nightly', id: 'x1', node_index: 1 })))
      .toBe('/project/infra/task/nightly/exec/x1?node=1');
  });

  it('keeps the selected evaluation and build in the query and drops empty values', () => {
    expect(String(links.task('p', 't', 'e1'))).toBe('/project/p/task/t?eval=e1');
    expect(String(links.task('p', 't', null))).toBe('/project/p/task/t');
    expect(String(links.evaluationLog('p', 'e1', { build: 'b1' }))).toBe('/project/p/log/e1?build=b1');
    expect(String(links.evaluationLog('p', 'e1', { package: 'b1' }))).toBe('/project/p/log/e1?package=b1');
    expect(String(links.evaluationLog('p', 'e1', { messages: true }))).toBe('/project/p/log/e1?eval=1');
  });

  it('opens a pending build on its own page, not on the first build of its evaluation', () => {
    expect(String(links.boardPendingJob({ evaluation_id: 'e1', build_id: 'b2' }))).toBe('/board/pending/b2');
    expect(String(links.boardPendingJob({ evaluation_id: 'e1', cluster_job: 'c1' }))).toBe('/board/clusters/c1');
  });

  it('shows a job of a cluster as its cluster job', () => {
    expect(String(links.boardJobOrCluster('d1', 'c1'))).toBe('/board/clusters/c1');
    expect(String(links.boardJobOrCluster('d1', null))).toBe('/board/jobs/d1');
    expect(links.boardJobOrCluster(null, undefined)).toBeUndefined();
  });

  it('Read evaluation from old `evalId` query parameter of bookmarked links', () => {
    expect(readBuildOrigin(convertToParamMap({ evalId: 'e1' }))).toEqual({ task: null, evaluation: 'e1' });
    expect(readBuildOrigin(convertToParamMap({ eval: 'e2', evalId: 'e1' })).evaluation).toBe('e2');
  });

  it('rejects next paths that point to another site', () => {
    const next = (value: string) => readInAppNextPathFromRouteSnapshot({ queryParamMap: convertToParamMap({ next: value }) } as ActivatedRouteSnapshot);
    expect(next('/projects?tab=1')).toBe('/projects?tab=1');
    expect(next('//evil.example')).toBeNull();
    expect(next('/\\evil.example')).toBeNull();
    expect(next('https://evil.example')).toBeNull();
  });
});
