/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, afterRenderEffect } from '@angular/core';

export function scrollActiveLinkToMiddleAfterRenderAndResize(
  activeLink: Signal<HTMLElement | null>,
  rowElement: () => HTMLElement | undefined,
  linkItems: Signal<unknown>,
): void {
  afterRenderEffect((onCleanup) => {
    linkItems();
    const link = activeLink();
    const row = rowElement();
    if (!link) return;
    const rowFitsWithoutScrolling = () => !!row && row.clientWidth > 0 && row.scrollWidth <= row.clientWidth;
    const scrollActiveLinkToMiddle = () => rowFitsWithoutScrolling() || link.scrollIntoView?.({ block: 'nearest', inline: 'center' });
    if (typeof ResizeObserver === 'undefined') return scrollActiveLinkToMiddle();
    const resizeObserver = new ResizeObserver(scrollActiveLinkToMiddle);
    resizeObserver.observe(link);
    if (row) resizeObserver.observe(row);
    onCleanup(() => resizeObserver.disconnect());
  });
}
