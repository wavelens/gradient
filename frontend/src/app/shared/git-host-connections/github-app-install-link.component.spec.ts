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
  return (fixture.nativeElement as HTMLElement).querySelector<HTMLAnchorElement>('[data-testid="github-app-install"]');
}

describe('GithubAppInstallLinkComponent', () => {
  it('open the install page of the app in the same tab', () => {
    const link = render('https://github.com/apps/gradient-ci/installations/new');
    expect(link?.getAttribute('href')).toBe('https://github.com/apps/gradient-ci/installations/new');
    expect(link?.hasAttribute('target')).toBe(false);
  });

  it('shows nothing when no install page is available', () => {
    expect(render(null)).toBeNull();
  });
});
