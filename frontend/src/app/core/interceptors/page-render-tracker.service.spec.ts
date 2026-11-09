/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { PageRenderTracker } from './page-render-tracker.service';

@Component({ template: '' })
class BlankPage {}

describe('PageRenderTracker', () => {
  let tracker: PageRenderTracker;
  let router: Router;

  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideRouter([{ path: 'page', component: BlankPage }])] });
    tracker = TestBed.inject(PageRenderTracker);
    router = TestBed.inject(Router);
  });

  it('treats the time before the first render of the app as page rendering', () => {
    expect(tracker.isPageStillRendering()).toBe(true);
    TestBed.tick();
    expect(tracker.isPageStillRendering()).toBe(false);
  });

  it('keeps page rendering after a navigation ends until the next render', async () => {
    TestBed.tick();
    await router.navigateByUrl('/page');
    expect(router.currentNavigation()).toBeNull();
    expect(tracker.isPageStillRendering()).toBe(true);
    TestBed.tick();
    expect(tracker.isPageStillRendering()).toBe(false);
  });
});
