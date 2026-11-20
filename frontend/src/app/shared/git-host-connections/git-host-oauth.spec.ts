/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import type { GiteaOAuthAppConfig } from '@core/models';
import { ConfigService } from '@core/services/config.service';
import { GitHostOAuthReturnBannerComponent } from './git-host-oauth-return-banner.component';
import { GitHostOAuthSignInComponent } from './git-host-oauth-sign-in.component';

const codeberg: GiteaOAuthAppConfig = { git_host_type: 'forgejo', instance_base_url: 'https://codeberg.org' };

function renderSignIn(app: GiteaOAuthAppConfig | null, names: string[] = []) {
  TestBed.configureTestingModule({
    imports: [GitHostOAuthSignInComponent],
    providers: [{ provide: ConfigService, useValue: { giteaOauthAppConfig: app } }],
  });
  const fixture = TestBed.createComponent(GitHostOAuthSignInComponent);
  fixture.componentRef.setInput('projectName', 'acme');
  fixture.componentRef.setInput('existingNames', names);
  fixture.detectChanges();
  return { el: fixture.nativeElement as HTMLElement, component: fixture.componentInstance };
}

function renderBanner(query: Record<string, string>) {
  TestBed.configureTestingModule({
    imports: [GitHostOAuthReturnBannerComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { queryParamMap: convertToParamMap(query) } } },
    ],
  });
  const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
  const fixture = TestBed.createComponent(GitHostOAuthReturnBannerComponent);
  fixture.detectChanges();
  const el = fixture.nativeElement as HTMLElement;
  return { navigate, banner: () => el.querySelector('[data-testid="oauth-outcome-banner"]')?.textContent?.trim() };
}

describe('GitHostOAuthSignInComponent', () => {
  it('offers no sign-in without an OAuth app on the server', () => {
    expect(renderSignIn(null).el.querySelector('[data-testid="oauth-sign-in"]')).toBeNull();
  });

  it('names the host and builds the connect URL with a free name from the host', () => {
    const { el, component } = renderSignIn(codeberg, ['codeberg']);
    expect(el.querySelector('[data-testid="oauth-sign-in"]')?.textContent).toContain('Sign in with Forgejo (codeberg.org)');
    expect(component.connectUrl()).toBe('/api/v1/projects/acme/git-host-connections/oauth/connect?name=codeberg-2');
  });
});

describe('GitHostOAuthReturnBannerComponent', () => {
  it('reports the connection and installed webhooks, then removes the result from the URL', () => {
    const { banner, navigate } = renderBanner({ oauth_connected: 'codeberg', webhooks_installed: '2' });
    expect(banner()).toContain('Connected codeberg. Installed webhooks in 2 repositories.');
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { oauth_connected: null, webhooks_installed: null, oauth_error: null }, replaceUrl: true }),
    );
  });

  it('reports the connection failure and substitutes generic text for an unknown code', () => {
    expect(renderBanner({ oauth_error: 'exists' }).banner()).toContain('There is already a connection with this name');
    TestBed.resetTestingModule();
    expect(renderBanner({ oauth_error: 'teapot' }).banner()).toContain('Failed to connect to the Git host. Try again.');
  });

  it('shows nothing when the URL has no OAuth result', () => {
    const { banner, navigate } = renderBanner({});
    expect(banner()).toBeUndefined();
    expect(navigate).not.toHaveBeenCalled();
  });
});
