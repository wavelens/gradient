/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Directive, ElementRef, effect, inject, input, output } from '@angular/core';

/// Emits whenever the host scrolls into view. A new `grInViewKey` re-observes,
/// so a sentinel that is still visible after the list grew fires again.
@Directive({
  selector: '[grInView]',
  standalone: true,
})
export class InViewDirective {
  grInView = output<void>();
  grInViewKey = input<unknown>();
  grInViewMargin = input('200px');

  private host = inject<ElementRef<Element>>(ElementRef);

  constructor() {
    effect((onCleanup) => {
      this.grInViewKey();
      const rootMargin = this.grInViewMargin();
      if (typeof IntersectionObserver === 'undefined') return;
      const observer = new IntersectionObserver(
        (entries) => entries.some((e) => e.isIntersecting) && this.grInView.emit(),
        { rootMargin },
      );
      observer.observe(this.host.nativeElement);
      onCleanup(() => observer.disconnect());
    });
  }
}
