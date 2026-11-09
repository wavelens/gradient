/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DestroyRef } from '@angular/core';
import { observeElementResizeUntilDestroyed } from './observe-element-resize-until-destroyed';

class FakeResizeObserver {
  static latest: FakeResizeObserver;
  disconnect = vi.fn();
  observe = vi.fn();
  constructor(readonly callback: (entries: { contentRect: { width: number; height: number } }[]) => void) {
    FakeResizeObserver.latest = this;
  }
  resizeTo(width: number, height: number): void {
    this.callback([{ contentRect: { width, height } }]);
  }
}

function fakeDestroyRef() {
  const callbacks: (() => void)[] = [];
  const ref = { destroyed: false, onDestroy: (callback: () => void) => { callbacks.push(callback); return () => undefined; } };
  return { ref: ref as unknown as DestroyRef, destroy: () => callbacks.forEach((callback) => callback()) };
}

describe('observeElementResizeUntilDestroyed', () => {
  beforeEach(() => vi.stubGlobal('ResizeObserver', FakeResizeObserver));
  afterEach(() => vi.unstubAllGlobals());

  it('calls back on a real size change, not on the first measure or an unchanged size and stops on destroy', () => {
    const onResize = vi.fn();
    const { ref, destroy } = fakeDestroyRef();
    observeElementResizeUntilDestroyed(document.createElement('div'), ref, onResize);
    const observer = FakeResizeObserver.latest;

    observer.resizeTo(1440, 900);
    observer.resizeTo(1440, 900);
    expect(onResize).not.toHaveBeenCalled();

    observer.resizeTo(375, 700);
    expect(onResize).toHaveBeenCalledTimes(1);

    destroy();
    expect(observer.disconnect).toHaveBeenCalled();
  });
});
