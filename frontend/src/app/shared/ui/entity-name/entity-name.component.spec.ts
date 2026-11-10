/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { EntityNameComponent } from './entity-name.component';
import { doesSearchQueryMatchEntityNameOrSlug, entityToPickerOption, formatEntityLabel } from './entity-name';

function render(inputs: Record<string, unknown>): HTMLElement {
  const fixture = TestBed.createComponent(EntityNameComponent);
  for (const [key, value] of Object.entries(inputs)) fixture.componentRef.setInput(key, value);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

describe('gr-entity-name', () => {
  it('shows the display name with the slug beside it', () => {
    const root = render({ kind: 'cache', displayName: 'MyCache', name: 'testcache' });
    expect(root.querySelector('.entity-name__display')?.textContent).toBe('MyCache');
    expect(root.querySelector('.entity-name__slug')?.textContent).toBe('testcache');
    expect(root.querySelector('gr-icon')).not.toBeNull();
    expect(root.getAttribute('title')).toBe('MyCache (testcache)');
  });

  it('shows the slug once when no other display name exists', () => {
    const root = render({ displayName: 'testcache', name: 'testcache' });
    expect(root.querySelector('.entity-name__slug')).toBeNull();
    expect(root.textContent?.trim()).toBe('testcache');
  });

  it('names the owning team when given', () => {
    const root = render({ displayName: 'Web Shop', name: 'shop', ownerTeamDisplayName: 'Platform' });
    expect(root.querySelector('.entity-name__owner')?.textContent).toContain('Platform');
  });
});

describe('entity label helpers', () => {
  const cache = { display_name: 'MyCache', name: 'testcache' };

  it('writes both names for text such as toasts and picker options', () => {
    expect(formatEntityLabel(cache)).toBe('MyCache (testcache)');
    expect(formatEntityLabel({ display_name: '', name: 'testcache' })).toBe('testcache');
    expect(entityToPickerOption(cache)).toEqual({ label: 'MyCache (testcache)', value: 'testcache' });
    expect(entityToPickerOption(cache, 'id-1')).toEqual({ label: 'MyCache (testcache)', value: 'id-1' });
  });

  it('matches a picker query on the display name or the slug', () => {
    expect(doesSearchQueryMatchEntityNameOrSlug(cache, 'mycache')).toBe(true);
    expect(doesSearchQueryMatchEntityNameOrSlug(cache, 'testc')).toBe(true);
    expect(doesSearchQueryMatchEntityNameOrSlug(cache, 'other')).toBe(false);
  });
});
