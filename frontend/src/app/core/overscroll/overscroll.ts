/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export const OVERSCROLL_BOTTOM = 'overscroll-bottom';

// Browsers paint the overscroll area with the root's single background color, so it follows the nearer edge.
export function followOverscroll(win: Window): () => void {
  const root = win.document.documentElement;
  const update = (): void => {
    const max = Math.max(0, root.scrollHeight - win.innerHeight);
    root.classList.toggle(OVERSCROLL_BOTTOM, win.scrollY * 2 > max);
  };
  update();
  win.addEventListener('scroll', update, { passive: true });
  win.addEventListener('resize', update, { passive: true });
  return () => {
    win.removeEventListener('scroll', update);
    win.removeEventListener('resize', update);
  };
}
