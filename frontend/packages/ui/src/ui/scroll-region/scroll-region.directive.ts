/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DestroyRef, Directive, ElementRef, afterNextRender, inject, signal } from '@angular/core';

/// Keyboard users can focus the scroll container during content overflow. Arrow keys scroll the container.
@Directive({
  selector: '[grScrollRegion]',
  standalone: true,
  host: { '[attr.tabindex]': 'overflowing() ? 0 : null' },
})
export class ScrollRegionDirective {
  protected readonly overflowing = signal(false);

  constructor() {
    const element = inject<ElementRef<HTMLElement>>(ElementRef).nativeElement;
    const measure = () =>
      this.overflowing.set(element.scrollWidth > element.clientWidth || element.scrollHeight > element.clientHeight);
    const destroyRef = inject(DestroyRef);
    afterNextRender(() => {
      measure();
      if (typeof ResizeObserver === 'undefined') return;
      const resize = new ResizeObserver(measure);
      const observeAll = () => {
        resize.observe(element);
        for (const child of Array.from(element.children)) resize.observe(child);
        measure();
      };
      const children = new MutationObserver(observeAll);
      children.observe(element, { childList: true });
      observeAll();
      destroyRef.onDestroy(() => {
        resize.disconnect();
        children.disconnect();
      });
    });
  }
}
