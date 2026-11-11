/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import type { ProjectReadinessResponse, WaitingReason } from '@core/models';
import { ProjectsService } from '@core/services/projects.service';
import { TeamsService } from '@core/services/teams.service';
import { NotifyService } from '@core/services/notify.service';
import { type WaitingAccess, waitingAccessFrom } from '@shared/evaluation';
import { WaitingNoticeComponent } from './waiting-notice.component';

const OUTSIDER: WaitingAccess = waitingAccessFrom([], false, false);
const ADMIN: WaitingAccess = waitingAccessFrom(['viewProject', 'manageWorkers', 'manageSubscriptions'], true, false);

const MISSING_CACHE_AND_WORKERS: ProjectReadinessResponse = {
  ready: false,
  workers: { ready: false, registered: 0, teams: [], teams_granted_without_workers: [], teams_you_can_grant: [] },
  cache: { ready: false, subscriptions: [] },
  repository: { status: 'verified', tasks: [{ name: 'web', display_name: 'Web App', status: 'verified' }] },
};

function render(reason: WaitingReason, access: WaitingAccess, banner = false) {
  const getProjectReadiness = vi.fn(() => of(MISSING_CACHE_AND_WORKERS));
  TestBed.configureTestingModule({
    imports: [WaitingNoticeComponent],
    providers: [
      provideRouter([]),
      { provide: ProjectsService, useValue: { getProjectReadiness } },
      { provide: TeamsService, useValue: {} },
      { provide: NotifyService, useValue: { success: vi.fn(), failure: vi.fn() } },
    ],
  });
  const fixture = TestBed.createComponent(WaitingNoticeComponent);
  fixture.componentRef.setInput('reason', reason);
  fixture.componentRef.setInput('project', 'acme');
  fixture.componentRef.setInput('access', access);
  fixture.componentRef.setInput('banner', banner);
  fixture.detectChanges();
  return { fixture, root: fixture.nativeElement as HTMLElement, getProjectReadiness };
}

describe('WaitingNoticeComponent', () => {
  it('links a project admin straight to the page that fixes the wait', () => {
    const { root } = render({ kind: 'cache_storage_full' }, ADMIN);
    expect(root.textContent).toContain('Cache storage full');
    expect(root.querySelector('a[grButton]')?.getAttribute('href')).toBe('/project/acme/caches');
  });

  it('shows the readiness rows with their fixes for a missing cache instead of a generic button', () => {
    const { root, getProjectReadiness } = render({ kind: 'no_cache' }, ADMIN, true);
    expect(getProjectReadiness).toHaveBeenCalledWith('acme');
    expect(root.querySelector('[data-testid="readiness-cache"]')?.textContent).toContain('None writable');
    expect(root.textContent).not.toContain('Add a cache');
    const hrefs = Array.from(root.querySelectorAll('a[grButton]')).map((link) => link.getAttribute('href'));
    expect(hrefs).toEqual(['/project/acme/workers', '/project/acme/caches']);
  });

  it('shows the readiness rows while the project has no connected worker', () => {
    const { root } = render({ kind: 'workers', unmet: [], connected_workers: 0, available_architectures: [] }, ADMIN);
    expect(root.querySelector('[data-testid="readiness-workers"]')?.textContent).toContain('No workers');
    expect(Array.from(root.querySelectorAll('a[grButton]')).filter((link) => link.textContent?.includes('Register worker'))).toHaveLength(1);
  });

  it('shows a visitor outside the project whom to ask, with no link and no readiness request', () => {
    const { root, getProjectReadiness } = render({ kind: 'no_cache' }, OUTSIDER, true);
    expect(root.querySelector('gr-message-banner')).not.toBeNull();
    expect(root.querySelector('a[grButton]')).toBeNull();
    expect(root.textContent).toContain('Ask a project admin');
    expect(getProjectReadiness).not.toHaveBeenCalled();
  });

  it('offers the pending job next to the fix, or alone when there is nothing to fix', () => {
    const { fixture, root } = render({ kind: 'no_cache' }, OUTSIDER);
    fixture.componentRef.setInput('pendingJobLink', TestBed.inject(Router).createUrlTree(['/board', 'pending', 'e1']));
    fixture.detectChanges();
    expect(Array.from(root.querySelectorAll('a[grButton]')).map((link) => link.getAttribute('href'))).toEqual(['/board/pending/e1']);
    expect(root.textContent).toContain('Ask a project admin');
  });

  it('starts a manual exec job from its button', () => {
    const { fixture, root } = render({ kind: 'manual_start' }, waitingAccessFrom([], true, false));
    const started = vi.fn();
    fixture.componentInstance.start.subscribe(started);
    root.querySelector<HTMLButtonElement>('button[grButton]')?.click();
    expect(started).toHaveBeenCalledOnce();
  });
});
