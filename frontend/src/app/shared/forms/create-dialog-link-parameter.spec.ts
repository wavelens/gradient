/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { links } from '@shared/routing';
import { openCreateDialogFromUrlIfAllowed, withCreateDialogQueryParam } from './create-dialog-link-parameter';

const opened = vi.fn();

@Component({ standalone: true, template: '', changeDetection: ChangeDetectionStrategy.Eager })
class ListPageComponent {
  constructor() {
    openCreateDialogFromUrlIfAllowed(() => allowed, opened);
  }
}

let allowed = true;

async function arrive(url: string) {
  TestBed.configureTestingModule({ providers: [provideRouter([{ path: 'projects', component: ListPageComponent }])] });
  const router = TestBed.inject(Router);
  const fixture = TestBed.createComponent(ListPageComponent);
  await router.navigateByUrl(url);
  fixture.detectChanges();
  await fixture.whenStable();
  return router;
}

describe('create dialog link parameter', () => {
  beforeEach(() => {
    opened.mockClear();
    allowed = true;
  });

  it('adds the parameter to a list link', () => {
    expect(withCreateDialogQueryParam(links.projects()).toString()).toBe('/projects?create=1');
  });

  it('opens the dialog once and removes the parameter from the address', async () => {
    const router = await arrive('/projects?create=1');
    expect(opened).toHaveBeenCalledTimes(1);
    expect(router.url).toBe('/projects');
  });

  it('leaves the dialog closed for a user who may not create', async () => {
    allowed = false;
    const router = await arrive('/projects?create=1');
    expect(opened).not.toHaveBeenCalled();
    expect(router.url).toBe('/projects');
  });

  it('does nothing without the parameter', async () => {
    await arrive('/projects');
    expect(opened).not.toHaveBeenCalled();
  });
});
