/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { RepositoryFetchStatus } from '@core/models';
import { RepositoryFetchBannerComponent } from './repository-fetch-banner.component';
import { links } from '@shared/routing';

function render(status: RepositoryFetchStatus | null, canEdit: boolean): HTMLElement {
  TestBed.configureTestingModule({ imports: [RepositoryFetchBannerComponent], providers: [provideRouter([])] });
  const fixture = TestBed.createComponent(RepositoryFetchBannerComponent);
  fixture.componentRef.setInput('status', status);
  fixture.componentRef.setInput('canEdit', canEdit);
  fixture.componentRef.setInput('fixLink', links.taskPage('p', 't', 'settings'));
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

const failed: RepositoryFetchStatus = {
  checked_at: '2026-01-01T10:00:00',
  ok_at: null,
  failed: true,
  error: 'git@host: Permission denied (publickey).',
};

describe('RepositoryFetchBannerComponent', () => {
  it('shows the git error, a fix and the settings link to editors', () => {
    const el = render(failed, true);
    expect(el.textContent).toContain("can't fetch the repository");
    expect(el.textContent).toContain('Permission denied (publickey)');
    expect(el.textContent).toContain('deploy key');
    expect(el.querySelector('a')?.getAttribute('href')).toBe('/project/p/task/t/settings');
  });

  it('tells read-only members whom to ask', () => {
    const el = render({ ...failed, error: null }, false);
    expect(el.textContent).toContain('Ask a project admin');
    expect(el.querySelector('a')).toBeNull();
  });

  it('stays hidden after a successful fetch', () => {
    const el = render({ checked_at: '2026-01-01T10:00:00', ok_at: '2026-01-01T10:00:00', failed: false, error: null }, true);
    expect(el.querySelector('[data-testid="repository-fetch-failed"]')).toBeNull();
  });
});
