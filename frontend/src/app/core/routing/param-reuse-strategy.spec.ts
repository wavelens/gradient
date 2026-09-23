/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ActivatedRouteSnapshot, Params, Route } from '@angular/router';
import { ParamReuseStrategy } from './param-reuse-strategy';

const TASK: Route = { path: 'project/:project/task/:task' };
const CACHE: Route = { path: 'caches/:cache' };

function snapshot(routeConfig: Route | null, params: Params, queryParams: Params = {}): ActivatedRouteSnapshot {
  return { routeConfig, params, queryParams } as unknown as ActivatedRouteSnapshot;
}

describe('ParamReuseStrategy', () => {
  const strategy = new ParamReuseStrategy();

  it('re-creates a route whose path params changed', () => {
    const from = snapshot(TASK, { project: 'infra', task: 'hosts' });
    const to = snapshot(TASK, { project: 'infra', task: 'docs' });
    expect(strategy.shouldReuseRoute(to, from)).toBe(false);
  });

  it('reuses a route with the same path params, whatever the query params', () => {
    const from = snapshot(TASK, { project: 'infra', task: 'hosts' });
    const to = snapshot(TASK, { project: 'infra', task: 'hosts' }, { eval: 'e2' });
    expect(strategy.shouldReuseRoute(to, from)).toBe(true);
  });

  it('never reuses across route configs', () => {
    expect(strategy.shouldReuseRoute(snapshot(CACHE, { cache: 'main' }), snapshot(TASK, { cache: 'main' }))).toBe(false);
  });

  it('treats a param missing on one side as a change', () => {
    expect(strategy.shouldReuseRoute(snapshot(CACHE, { cache: 'main', extra: 'x' }), snapshot(CACHE, { cache: 'main' }))).toBe(false);
  });
});
