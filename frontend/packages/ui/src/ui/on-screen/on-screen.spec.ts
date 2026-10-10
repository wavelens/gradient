/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { injectOnScreen } from './on-screen';

@Component({ selector: 'gr-probe', standalone: true, template: '' })
class ProbeComponent {
  onScreen = injectOnScreen();
}

describe('injectOnScreen', () => {
  const original = globalThis.IntersectionObserver;
  let report: (entries: Partial<IntersectionObserverEntry>[]) => void;
  const unobserve = vi.fn();

  beforeEach(() => {
    globalThis.IntersectionObserver = class {
      constructor(callback: (entries: Partial<IntersectionObserverEntry>[]) => void) { report = callback; }
      observe = vi.fn();
      unobserve = unobserve;
    } as unknown as typeof IntersectionObserver;
  });

  afterEach(() => { globalThis.IntersectionObserver = original; });

  it('follows the host element on and off the screen, then lets go of it', () => {
    const fixture = TestBed.createComponent(ProbeComponent);
    const target = fixture.nativeElement as Element;
    expect(fixture.componentInstance.onScreen()).toBe(true);

    report([{ target, isIntersecting: false }]);
    expect(fixture.componentInstance.onScreen()).toBe(false);

    report([{ target, isIntersecting: true }]);
    expect(fixture.componentInstance.onScreen()).toBe(true);

    fixture.destroy();
    expect(unobserve).toHaveBeenCalledWith(target);
  });
});
