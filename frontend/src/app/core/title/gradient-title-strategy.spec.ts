/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ActivatedRouteSnapshot } from '@angular/router';
import { composeTitle, findEntityName } from './gradient-title-strategy';

interface FakeSnapshot {
  data: Record<string, unknown>;
  children: FakeSnapshot[];
}

function snap(data: Record<string, unknown> = {}, children: FakeSnapshot[] = []): FakeSnapshot {
  return { data, children };
}

function find(s: FakeSnapshot): string | undefined {
  return findEntityName(s as unknown as ActivatedRouteSnapshot);
}

describe('composeTitle', () => {
  it('uses entity + page + brand when both are present', () => {
    expect(composeTitle('Demo', 'Project Settings')).toBe('Demo · Project Settings · Gradient');
  });

  it('collapses redundant entity-only page titles', () => {
    // The detail page's static title ("Project"/"Cache"/"Organization") is implied
    // by the entity name itself, so it drops out to avoid "Demo · Project · Gradient".
    expect(composeTitle('Demo', 'Project')).toBe('Demo · Gradient');
    expect(composeTitle('Demo', 'Cache')).toBe('Demo · Gradient');
    expect(composeTitle('Demo', 'Organization')).toBe('Demo · Gradient');
  });

  it('falls back to page + brand when no entity is in the tree', () => {
    expect(composeTitle(undefined, 'Sign In')).toBe('Sign In · Gradient');
  });

  it('falls back to brand alone when there is nothing else', () => {
    expect(composeTitle(undefined, undefined)).toBe('Gradient');
  });
});

describe('findEntityName', () => {
  it('returns the project display_name from resolved data', () => {
    const tree = snap({}, [
      snap({ projectAccess: { project: { display_name: 'Test Gradient' } } }),
    ]);
    expect(find(tree)).toBe('Test Gradient');
  });

  it('returns the cache display_name from resolved data', () => {
    const tree = snap({}, [
      snap({ cacheAccess: { cache: { display_name: 'main' } } }),
    ]);
    expect(find(tree)).toBe('main');
  });

  it('returns the organization display_name from resolved data', () => {
    const tree = snap({ organizationAccess: { organization: { display_name: 'Wavelens' } } });
    expect(find(tree)).toBe('Wavelens');
  });

  it('returns undefined when no entity data is in the tree', () => {
    expect(find(snap())).toBeUndefined();
  });

  it('returns undefined when an organizationAccess resolves to null (404)', () => {
    const tree = snap({ organizationAccess: { organization: null } });
    expect(find(tree)).toBeUndefined();
  });
});
