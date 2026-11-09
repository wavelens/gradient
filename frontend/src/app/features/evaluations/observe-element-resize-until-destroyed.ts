/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DestroyRef } from '@angular/core';

export function observeElementResizeUntilDestroyed(element: Element, destroyRef: DestroyRef, onResize: () => void): void {
  if (typeof ResizeObserver === 'undefined' || destroyRef.destroyed) return;
  let lastSize = '';
  const observer = new ResizeObserver(([entry]) => {
    const size = `${Math.round(entry.contentRect.width)}x${Math.round(entry.contentRect.height)}`;
    if (lastSize && size !== lastSize) onResize();
    lastSize = size;
  });
  observer.observe(element);
  destroyRef.onDestroy(() => observer.disconnect());
}
