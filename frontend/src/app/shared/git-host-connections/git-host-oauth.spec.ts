/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import type { GiteaOAuthAppConfig } from '@core/models';
import { ConfigService } from '@core/services/config.service';
import { environment } from '@environments/environment';
import { GitHostOAuthReturnBannerComponent } from './git-host-oauth-return-banner.component';
import { GitHostOAuthSignInComponent } from './git-host-oauth-sign-in.component';

const codeberg: GiteaOAuthAppConfig = { git_host_type: 'forgejo', instance_base_url: 'https://codeberg.org' };
const completeOAuth2ProjectConnectionUrl = `${environment.apiUrl}/projects/acme/git-host-connections/oauth/complete`;

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
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: { snapshot: { queryParamMap: convertToParamMap(query) } } },
    ],
  });
  const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
  const fixture = TestBed.createComponent(GitHostOAuthReturnBannerComponent);
  fixture.componentRef.setInput('projectName', 'acme');
  fixture.detectChanges();
  const el = fixture.nativeElement as HTMLElement;
  return {
    fixture,
    navigate,
    http: TestBed.inject(HttpTestingController),
    banner: () => el.querySelector('[data-testid="oauth-outcome-banner"]')?.textContent?.trim(),
  };
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
  it('send code and state to complete route once, remove them from URL before response arrives', () => {
    const { fixture, navigate, http } = renderBanner({ code: 'code-1', state: 'state-1' });
    const request = http.expectOne(completeOAuth2ProjectConnectionUrl);
    expect(request.request.method).toBe('POST');
    expect(request.request.body).toEqual({ code: 'code-1', state: 'state-1' });
    expect(navigate).toHaveBeenCalledTimes(1);
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { code: null, state: null, oauth_error: null }, replaceUrl: true }),
    );
    request.flush({ error: false, message: { name: 'codeberg', webhooks_installed: 0 } });
    fixture.detectChanges();
    http.verify();
  });

  it('reports the connection and installed webhooks, then removes the result from the URL', () => {
    const { fixture, banner, http } = renderBanner({ code: 'code-1', state: 'state-1' });
    http.expectOne(completeOAuth2ProjectConnectionUrl).flush({ error: false, message: { name: 'codeberg', webhooks_installed: 2 } });
    fixture.detectChanges();
    expect(banner()).toContain('Connected codeberg. Installed webhooks in 2 repositories.');
  });

  it('shows the server refusal text when the OAuth exchange returns 400 and sends no further request', () => {
    const { fixture, banner, http } = renderBanner({ code: 'code-1', state: 'state-1' });
    http.expectOne(completeOAuth2ProjectConnectionUrl).flush(
      { error: true, message: 'The connection attempt was not started in this browser session.' },
      { status: 400, statusText: 'Bad Request' },
    );
    fixture.detectChanges();
    expect(banner()).toContain('The connection attempt was not started in this browser session.');
    http.verify();
  });

  it('explain an expired connection attempt without sending a request and remove the parameter', () => {
    const { banner, navigate, http } = renderBanner({ oauth_error: 'expired' });
    expect(banner()).toContain('The approval took over 10 minutes, or a different browser session approved the connection.');
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { code: null, state: null, oauth_error: null }, replaceUrl: true }),
    );
    http.verify();
  });

  it('reports the connection failure and substitutes generic text for an unknown code', () => {
    expect(renderBanner({ oauth_error: 'denied' }).banner()).toContain('The Git host denied access.');
    TestBed.resetTestingModule();
    expect(renderBanner({ oauth_error: 'teapot' }).banner()).toContain('Failed to connect to the Git host. Try again.');
  });

  it('shows nothing when the URL has no OAuth result', () => {
    const { banner, navigate, http } = renderBanner({});
    expect(banner()).toBeUndefined();
    expect(navigate).not.toHaveBeenCalled();
    http.verify();
  });
});
