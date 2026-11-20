/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ConfigService } from '@core/services/config.service';
import { GithubAppInstallLinkComponent } from './github-app-install-link.component';

function render(installUrl: string | null) {
  TestBed.configureTestingModule({
    imports: [GithubAppInstallLinkComponent],
    providers: [{ provide: ConfigService, useValue: { githubAppInstallUrl: installUrl } }],
  });
  const fixture = TestBed.createComponent(GithubAppInstallLinkComponent);
  fixture.detectChanges();
  const returns: unknown[] = [];
  fixture.componentInstance.returnedFromGitHub.subscribe(() => returns.push(true));
  const link = (fixture.nativeElement as HTMLElement).querySelector<HTMLAnchorElement>('[data-testid="github-app-install"]');
  return { fixture, link, returns };
}

describe('GithubAppInstallLinkComponent', () => {
  it('opens the install page for the App in a new tab', () => {
    const { link } = render('https://github.com/apps/gradient-ci/installations/new');
    expect(link?.getAttribute('href')).toBe('https://github.com/apps/gradient-ci/installations/new');
    expect(link?.getAttribute('target')).toBe('_blank');
  });

  it('shows nothing when no install page is available', () => {
    expect(render(null).link).toBeNull();
  });

  it('reports one return to the window after a click', () => {
    const { link, returns } = render('https://github.com/apps/gradient-ci/installations/new');
    window.dispatchEvent(new Event('focus'));
    expect(returns).toHaveLength(0);
    link!.addEventListener('click', (event) => event.preventDefault());
    link!.click();
    window.dispatchEvent(new Event('focus'));
    window.dispatchEvent(new Event('focus'));
    expect(returns).toHaveLength(1);
  });
});
