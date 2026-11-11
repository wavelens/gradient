/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import type { AccessState, GiteaOAuthAppConfig } from '@core/models';
import { ConfigService } from '@core/services/config.service';
import { ForgejoOAuthConnectionComponent } from './forgejo-oauth-connection.component';

const writable: AccessState = { managed: false, canEdit: true, canTrigger: true };
const readOnly: AccessState = { managed: false, canEdit: false, canTrigger: false };
const codeberg: GiteaOAuthAppConfig = { git_host_type: 'forgejo', instance_base_url: 'https://codeberg.org' };

function render(options: { app?: GiteaOAuthAppConfig | null; access?: AccessState; names?: string[]; query?: Record<string, string> } = {}) {
  TestBed.configureTestingModule({
    imports: [ForgejoOAuthConnectionComponent],
    providers: [
      provideRouter([]),
      { provide: ConfigService, useValue: { giteaOauthAppConfig: options.app === undefined ? codeberg : options.app } },
      { provide: ActivatedRoute, useValue: { snapshot: { queryParamMap: convertToParamMap(options.query ?? {}) } } },
    ],
  });
  const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
  const fixture = TestBed.createComponent(ForgejoOAuthConnectionComponent);
  fixture.componentRef.setInput('projectName', 'acme');
  fixture.componentRef.setInput('access', options.access ?? writable);
  fixture.componentRef.setInput('existingConnectionNames', options.names ?? []);
  fixture.detectChanges();
  const el = fixture.nativeElement as HTMLElement;
  const banner = () => el.querySelector('[data-testid="oauth-outcome-banner"]')?.textContent?.trim();
  return { fixture, el, component: fixture.componentInstance, navigate, banner };
}

describe('ForgejoOAuthConnectionComponent', () => {
  it('offers no OAuth2 connection without an OAuth2 app on the server', () => {
    const { el } = render({ app: null });
    expect(el.querySelector('[data-testid="oauth-connect-section"]')).toBeNull();
  });

  it('offers no OAuth2 connection to a member who cannot manage connections', () => {
    const { el } = render({ access: readOnly });
    expect(el.querySelector('[data-testid="oauth-connect-section"]')).toBeNull();
  });

  it('prefills the name from the host and builds the connect URL with name and display name', () => {
    const { el, component } = render();
    const button = Array.from(el.querySelectorAll('button')).find((b) => b.textContent?.includes('Connect with Forgejo (codeberg.org)'));
    button!.click();
    expect(component.isOAuth2ConnectionNameDialogOpen()).toBe(true);
    expect(component.name()).toBe('codeberg');
    component.displayName.set('Codeberg CI');
    expect(component.oauth2ConnectUrlWithNameAndDisplayName()).toBe(
      '/api/v1/projects/acme/git-host-connections/oauth/connect?name=codeberg&display_name=Codeberg+CI',
    );
  });

  it('rejects a name that the project already uses before the redirect', () => {
    const { fixture, component } = render({ names: ['codeberg'] });
    component.openOAuth2ConnectDialogWithDefaultNameFromHostname();
    component.validateNameAndNavigateToConnectUrl();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog .field-error')?.textContent?.trim()).toBe('This owner already has a connection with this name.');
  });

  it('reports the connection and the installed webhooks and removes the result from the URL', () => {
    const { banner, navigate } = render({ query: { oauth_connected: 'codeberg', webhooks_installed: '2' } });
    expect(banner()).toContain('Connected codeberg. Installed webhooks in 2 repositories.');
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { oauth_connected: null, webhooks_installed: null, oauth_error: null }, replaceUrl: true }),
    );
  });

  it('explains a failed connection and falls back to a generic text for an unknown code', () => {
    expect(render({ query: { oauth_error: 'exists' } }).banner()).toContain('There is already a connection with this name');
    TestBed.resetTestingModule();
    expect(render({ query: { oauth_error: 'teapot' } }).banner()).toContain('Connection to the Git host failed. Try again.');
  });
});
