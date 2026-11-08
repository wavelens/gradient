/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injector, afterNextRender, inject } from '@angular/core';
import { revealFirstInvalid } from '@gradient/ui/ui';

/// `gr-form-field` can mark its control invalid during render. Wait for the next render.
export function injectRevealFirstInvalid(): (root: () => ParentNode | null | undefined) => void {
  const injector = inject(Injector);
  return (root) => afterNextRender(() => {
    const element = root();
    if (element) revealFirstInvalid(element);
  }, { injector });
}
