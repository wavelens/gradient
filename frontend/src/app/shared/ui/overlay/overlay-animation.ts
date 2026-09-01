/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { OverlayRef } from '@angular/cdk/overlay';

export const LEAVING_CLASS = 'gr-overlay-leaving';

/// CDK tears an overlay down synchronously, so an exit animation needs the teardown
/// deferred. Stays synchronous when no animation will run, including reduced-motion.
function runExit(ref: OverlayRef, teardown: () => void): void {
  const pane = ref.overlayElement;
  const backdrop = ref.backdropElement;
  if (!pane) {
    teardown();
    return;
  }

  pane.classList.add(LEAVING_CLASS);
  backdrop?.classList.add(LEAVING_CLASS);

  const duration = parseFloat(getComputedStyle(pane).animationDuration || '0') * 1000;
  if (!(duration > 0)) {
    pane.classList.remove(LEAVING_CLASS);
    teardown();
    return;
  }

  let done = false;
  const finish = () => {
    if (done) return;
    done = true;
    pane.removeEventListener('animationend', finish);
    pane.classList.remove(LEAVING_CLASS);
    teardown();
  };

  pane.addEventListener('animationend', finish);
  window.setTimeout(finish, duration + 50);
}

export function detachAnimated(ref: OverlayRef | undefined | null, after?: () => void): void {
  if (!ref?.hasAttached()) return;
  runExit(ref, () => {
    ref.detach();
    after?.();
  });
}

export function disposeAnimated(ref: OverlayRef | undefined | null, after?: () => void): void {
  if (!ref?.hasAttached()) {
    ref?.dispose();
    after?.();
    return;
  }
  runExit(ref, () => {
    ref.dispose();
    after?.();
  });
}
