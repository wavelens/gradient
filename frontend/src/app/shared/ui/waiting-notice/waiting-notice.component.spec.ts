/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { WaitingReason } from '@core/models';
import type { WaitingAccess } from '@shared/evaluation';
import { WaitingNoticeComponent } from './waiting-notice.component';

function render(reason: WaitingReason, access: WaitingAccess, banner = false) {
  TestBed.configureTestingModule({ imports: [WaitingNoticeComponent], providers: [provideRouter([])] });
  const fixture = TestBed.createComponent(WaitingNoticeComponent);
  fixture.componentRef.setInput('reason', reason);
  fixture.componentRef.setInput('project', 'acme');
  fixture.componentRef.setInput('access', access);
  fixture.componentRef.setInput('banner', banner);
  fixture.detectChanges();
  return { fixture, root: fixture.nativeElement as HTMLElement };
}

describe('WaitingNoticeComponent', () => {
  it('links a project admin straight to the page that fixes the wait', () => {
    const { root } = render({ kind: 'no_cache' }, { manageWorkers: true, manageSubscriptions: true, trigger: true, superuser: false });
    expect(root.textContent).toContain('Cache not configured');
    expect(root.querySelector('a[grButton]')?.getAttribute('href')).toBe('/project/acme/caches');
  });

  it('shows a viewer whom to ask, with no link', () => {
    const { root } = render({ kind: 'no_cache' }, { manageWorkers: false, manageSubscriptions: false, trigger: false, superuser: false }, true);
    expect(root.querySelector('gr-message-banner')).not.toBeNull();
    expect(root.querySelector('a[grButton]')).toBeNull();
    expect(root.textContent).toContain('Ask a project admin');
  });

  it('starts a manual exec job from its button', () => {
    const { fixture, root } = render({ kind: 'manual_start' }, { manageWorkers: false, manageSubscriptions: false, trigger: true, superuser: false });
    const started = vi.fn();
    fixture.componentInstance.start.subscribe(started);
    root.querySelector<HTMLButtonElement>('button[grButton]')?.click();
    expect(started).toHaveBeenCalledOnce();
  });
});
