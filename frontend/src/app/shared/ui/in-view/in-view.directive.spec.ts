/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { vi } from 'vitest';
import { InViewDirective } from './in-view.directive';

type Callback = (entries: Partial<IntersectionObserverEntry>[]) => void;

class FakeObserver {
  static live: FakeObserver[] = [];
  observed: Element[] = [];
  constructor(readonly callback: Callback) { FakeObserver.live.push(this); }
  observe(el: Element) { this.observed.push(el); }
  disconnect() { FakeObserver.live = FakeObserver.live.filter(o => o !== this); }
}

@Component({
  standalone: true,
  imports: [InViewDirective],
  template: `<div class="sentinel" (grInView)="seen.set(seen() + 1)" [grInViewKey]="key()"></div>`,
})
class HostComponent {
  seen = signal(0);
  key = signal(0);
}

function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  return fixture;
}

describe('InViewDirective', () => {
  beforeEach(() => {
    FakeObserver.live = [];
    vi.stubGlobal('IntersectionObserver', FakeObserver);
  });
  afterEach(() => vi.unstubAllGlobals());

  it('emits when the element scrolls into view, not when it leaves', () => {
    const fixture = render();
    const [observer] = FakeObserver.live;
    expect(observer.observed[0]).toBe(fixture.nativeElement.querySelector('.sentinel'));
    observer.callback([{ isIntersecting: false }]);
    observer.callback([{ isIntersecting: true }]);
    expect(fixture.componentInstance.seen()).toBe(1);
  });

  /// An observer only reports changes, so a sentinel still in view after a
  /// page landed would never fire again without a fresh one.
  it('observes afresh when the key changes', () => {
    const fixture = render();
    fixture.componentInstance.key.set(1);
    fixture.detectChanges();
    expect(FakeObserver.live).toHaveLength(1);
    FakeObserver.live[0].callback([{ isIntersecting: true }]);
    expect(fixture.componentInstance.seen()).toBe(1);
  });
});
