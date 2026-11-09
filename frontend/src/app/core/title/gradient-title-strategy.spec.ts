/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient, withInterceptors } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { Title } from '@angular/platform-browser';
import {
  ActivatedRouteSnapshot,
  ParamMap,
  PRIMARY_OUTLET,
  Router,
  RouterStateSnapshot,
  TitleStrategy,
  convertToParamMap,
  provideRouter,
} from '@angular/router';
import { errorInterceptor } from '@core/interceptors/error.interceptor';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { GradientTitleStrategy, composeTitle, findFirstResolvedEntityInRouterSnapshotTree } from './gradient-title-strategy';

interface FakeSnapshot {
  data: Record<string, unknown>;
  children: FakeSnapshot[];
  paramMap: ParamMap;
  outlet: string;
}

function snap(data: Record<string, unknown> = {}, children: FakeSnapshot[] = [], params: Record<string, string> = {}): FakeSnapshot {
  return { data, children, paramMap: convertToParamMap(params), outlet: PRIMARY_OUTLET };
}

function find(s: FakeSnapshot): string | undefined {
  return findFirstResolvedEntityInRouterSnapshotTree(s as unknown as ActivatedRouteSnapshot)?.displayName;
}

describe('composeTitle', () => {
  it('uses entity + page + brand when both are present', () => {
    expect(composeTitle('Demo', 'Task Settings')).toBe('Demo · Task Settings · Gradient');
  });

  it('collapses redundant entity-only page titles', () => {
    // The detail page's static title ("Task"/"Cache"/"Project") is implied
    // by the entity name itself, so it drops out to avoid "Demo · Task · Gradient".
    expect(composeTitle('Demo', 'Task')).toBe('Demo · Gradient');
    expect(composeTitle('Demo', 'Cache')).toBe('Demo · Gradient');
    expect(composeTitle('Demo', 'Project')).toBe('Demo · Gradient');
  });

  it('falls back to page + brand when no entity is in the tree', () => {
    expect(composeTitle(undefined, 'Sign In')).toBe('Sign In · Gradient');
  });

  it('falls back to brand alone when there is nothing else', () => {
    expect(composeTitle(undefined, undefined)).toBe('Gradient');
  });
});

describe('GradientTitleStrategy', () => {
  it('follows a rename saved on a settings page without a new navigation', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const strategy = TestBed.inject(GradientTitleStrategy);
    const crumbs = TestBed.inject(BreadcrumbsService);
    const title = TestBed.inject(Title);
    crumbs.rememberProject('alpha', 'Alpha');
    const root = snap({}, [snap({ projectAccess: { project: { display_name: 'Alpha' } } }, [], { project: 'alpha' })]);
    strategy.updateTitle({ root } as unknown as RouterStateSnapshot);
    TestBed.tick();
    expect(title.getTitle()).toBe('Alpha · Gradient');

    crumbs.rememberProject('alpha', 'Alpha Renamed');
    TestBed.tick();
    expect(title.getTitle()).toBe('Alpha Renamed · Gradient');
  });

  it('can be created by the router', () => {
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: TitleStrategy, useClass: GradientTitleStrategy },
        provideHttpClient(withInterceptors([errorInterceptor])),
        provideHttpClientTesting(),
      ],
    });
    expect(() => TestBed.inject(Router)).not.toThrow();
  });
});

describe('findFirstResolvedEntityInRouterSnapshotTree', () => {
  it('names the task by its project and task url names', () => {
    const tree = snap({}, [snap({ taskAccess: { task: { display_name: 'Build' } } }, [], { project: 'alpha', task: 'build' })]);
    expect(findFirstResolvedEntityInRouterSnapshotTree(tree as unknown as ActivatedRouteSnapshot)).toEqual({
      kind: 'task',
      urlNames: ['alpha', 'build'],
      displayName: 'Build',
    });
  });

  it('returns the task display_name from resolved data', () => {
    const tree = snap({}, [
      snap({ taskAccess: { task: { display_name: 'Test Gradient' } } }),
    ]);
    expect(find(tree)).toBe('Test Gradient');
  });

  it('returns the cache display_name from resolved data', () => {
    const tree = snap({}, [
      snap({ cacheAccess: { cache: { display_name: 'main' } } }),
    ]);
    expect(find(tree)).toBe('main');
  });

  it('returns the project display_name from resolved data', () => {
    const tree = snap({ projectAccess: { project: { display_name: 'Wavelens' } } });
    expect(find(tree)).toBe('Wavelens');
  });

  it('returns the team display_name from resolved data', () => {
    const tree = snap({}, [snap({ teamAccess: { team: { display_name: 'Research' } } })]);
    expect(find(tree)).toBe('Research');
    expect(composeTitle(find(tree), 'Team')).toBe('Research · Gradient');
    expect(composeTitle(find(tree), 'Team Settings')).toBe('Research · Team Settings · Gradient');
  });

  it('returns undefined when no entity data is in the tree', () => {
    expect(find(snap())).toBeUndefined();
  });

  it('returns undefined when an projectAccess resolves to null (404)', () => {
    const tree = snap({ projectAccess: { project: null } });
    expect(find(tree)).toBeUndefined();
  });
});
