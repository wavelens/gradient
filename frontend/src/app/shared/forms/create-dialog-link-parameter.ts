/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { afterNextRender, inject } from '@angular/core';
import { Router, UrlTree } from '@angular/router';

const CREATE = 'create';

export function withCreateDialogQueryParam(link: UrlTree): UrlTree {
  return new UrlTree(link.root, { ...link.queryParams, [CREATE]: '1' }, link.fragment);
}

export function openCreateDialogFromUrlIfAllowed(canCreate: () => boolean, open: () => void): void {
  const router = inject(Router);
  afterNextRender(() => {
    const current = router.parseUrl(router.url);
    if (current.queryParams[CREATE] !== '1') return;
    delete current.queryParams[CREATE];
    router.navigateByUrl(current, { replaceUrl: true });
    if (canCreate()) open();
  });
}
