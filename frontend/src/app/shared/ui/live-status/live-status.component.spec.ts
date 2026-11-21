/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { LiveStatusComponent } from './live-status.component';

function renderLiveStatusBadgeWithConnectedAndReturnRootElement(connected: boolean): HTMLElement {
  const fixture = TestBed.createComponent(LiveStatusComponent);
  fixture.componentRef.setInput('connected', connected);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

describe('LiveStatusComponent', () => {
  it('show the Live badge while live updates stay connected', () => {
    const root = renderLiveStatusBadgeWithConnectedAndReturnRootElement(true);

    expect(root.querySelector('gr-badge')?.textContent?.trim()).toBe('Live');
    expect(root.querySelector('[role="status"]')?.getAttribute('aria-label')).toBe('Live updates connected');
  });

  it('warn that values can become stale while live updates reconnect', () => {
    const root = renderLiveStatusBadgeWithConnectedAndReturnRootElement(false);

    expect(root.querySelector('gr-badge')?.textContent?.trim()).toBe('Reconnecting');
    expect(root.querySelector('[role="status"]')?.getAttribute('aria-label')).toContain('Values can be stale');
  });
});
