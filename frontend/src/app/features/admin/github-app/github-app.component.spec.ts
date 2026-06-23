/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap } from '@angular/router';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of, throwError } from 'rxjs';
import { GithubAppComponent } from './github-app.component';
import { AdminService } from '@core/services/admin.service';

type MockedAdmin = {
  requestGithubAppManifest: ReturnType<typeof vi.fn>;
  fetchGithubAppCredentials: ReturnType<typeof vi.fn>;
};

describe('GithubAppComponent', () => {
  let admin: MockedAdmin;

  function setup(queryParams: Record<string, string> = {}) {
    admin = {
      requestGithubAppManifest: vi.fn(),
      fetchGithubAppCredentials: vi.fn(),
    };
    TestBed.configureTestingModule({
      imports: [GithubAppComponent],
      providers: [
        provideRouter([]),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: AdminService, useValue: admin },
        {
          provide: ActivatedRoute,
          useValue: {
            snapshot: { queryParamMap: convertToParamMap(queryParams) },
          },
        },
      ],
    });
  }

  it('shows the setup view when ready=1 is absent', () => {
    setup({});
    const fixture: ComponentFixture<GithubAppComponent> =
      TestBed.createComponent(GithubAppComponent);
    fixture.detectChanges();
    const setupBtn = fixture.nativeElement.querySelector(
      '[data-testid="github-app-create-button"]',
    );
    expect(setupBtn).toBeTruthy();
  });

  it('clicking create-button calls requestGithubAppManifest with host', () => {
    setup({});
    admin.requestGithubAppManifest.mockReturnValue(
      of({ manifest: {}, post_url: 'https://github.com/x', state: 's' }),
    );
    const fixture = TestBed.createComponent(GithubAppComponent);
    fixture.componentInstance.host.set('ghe.example.com');
    fixture.componentInstance.create();
    expect(admin.requestGithubAppManifest).toHaveBeenCalledWith('ghe.example.com');
  });

  it('renders credentials when ready=1 and the API returns them', async () => {
    setup({ ready: '1' });
    admin.fetchGithubAppCredentials.mockReturnValue(
      of({
        id: 7,
        slug: 'gradient',
        html_url: 'https://github.com/apps/gradient',
        pem: 'PEM',
        webhook_secret: 'whsec',
        client_id: 'cid',
        client_secret: 'csec',
      }),
    );
    const fixture = TestBed.createComponent(GithubAppComponent);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    const html: string = fixture.nativeElement.textContent;
    expect(html).toContain('PEM');
    expect(html).toContain('whsec');
    expect(html).toContain('cid');
  });

  it('shows a friendly error when credentials are no longer available', async () => {
    setup({ ready: '1' });
    admin.fetchGithubAppCredentials.mockReturnValue(
      throwError(() => ({ status: 404 })),
    );
    const fixture = TestBed.createComponent(GithubAppComponent);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    const banner = fixture.nativeElement.querySelector(
      '[data-testid="github-app-credentials-missing"]',
    );
    expect(banner).toBeTruthy();
  });

  describe('navigation guard while credentials are shown', () => {
    function withCredentials() {
      setup({ ready: '1' });
      admin.fetchGithubAppCredentials.mockReturnValue(
        of({
          id: 7,
          slug: 'gradient',
          html_url: 'https://github.com/apps/gradient',
          pem: 'PEM',
          webhook_secret: 'whsec',
          client_id: 'cid',
          client_secret: 'csec',
        }),
      );
      const fixture = TestBed.createComponent(GithubAppComponent);
      fixture.detectChanges();
      return fixture;
    }

    it('allows leaving the setup view without prompting', () => {
      const confirmSpy = vi.spyOn(window, 'confirm');
      setup({});
      const fixture = TestBed.createComponent(GithubAppComponent);
      fixture.detectChanges();
      expect(fixture.componentInstance.canDeactivate()).toBe(true);
      expect(confirmSpy).not.toHaveBeenCalled();
    });

    it('prompts and blocks leaving when the user cancels', () => {
      const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false);
      const fixture = withCredentials();
      expect(fixture.componentInstance.canDeactivate()).toBe(false);
      expect(confirmSpy).toHaveBeenCalledOnce();
    });

    it('prompts and allows leaving when the user confirms', () => {
      vi.spyOn(window, 'confirm').mockReturnValue(true);
      const fixture = withCredentials();
      expect(fixture.componentInstance.canDeactivate()).toBe(true);
    });

    it('flags beforeunload while credentials are shown', () => {
      const fixture = withCredentials();
      const event = new Event('beforeunload', { cancelable: true });
      fixture.componentInstance.onBeforeUnload(event as BeforeUnloadEvent);
      expect(event.defaultPrevented).toBe(true);
    });
  });
});
