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
import { EMPTY, Observable, Subject, of, throwError } from 'rxjs';
import type { Mock } from 'vitest';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { GithubAppComponent } from './github-app.component';
import { AdminService } from '@core/services/admin.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';

type MockedAdmin = {
  requestGithubAppManifest: ReturnType<typeof vi.fn>;
  fetchGithubAppCredentials: ReturnType<typeof vi.fn>;
};

describe('GithubAppComponent', () => {
  let admin: MockedAdmin;
  let notify: { success: ReturnType<typeof vi.fn>; failure: ReturnType<typeof vi.fn> };
  let confirm: Mock<(request: ConfirmRequest) => Observable<unknown>>;

  function setup(queryParams: Record<string, string> = {}) {
    admin = {
      requestGithubAppManifest: vi.fn(),
      fetchGithubAppCredentials: vi.fn(),
    };
    notify = { success: vi.fn(), failure: vi.fn() };
    confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
    vi.spyOn(window, 'confirm');
    TestBed.configureTestingModule({
      imports: [GithubAppComponent],
      providers: [
        provideRouter([]),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: AdminService, useValue: admin },
        { provide: NotifyService, useValue: notify },
        { provide: ConfirmService, useValue: { confirm } },
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

  it('reports a refused manifest request as a toast and enables the button again', () => {
    setup({});
    const failure = new ApiError('GitHub App is already configured', 409);
    admin.requestGithubAppManifest.mockReturnValue(throwError(() => failure));
    const fixture = TestBed.createComponent(GithubAppComponent);
    fixture.componentInstance.create();
    fixture.detectChanges();
    expect(notify.failure).toHaveBeenCalledWith('Setup for GitHub App did not start', failure);
    expect(fixture.nativeElement.querySelector('[data-testid="github-app-create-button"]').getAttribute('aria-busy')).toBeNull();
  });

  it('ignores a second click on Create on GitHub while the manifest request runs', () => {
    setup({});
    admin.requestGithubAppManifest.mockReturnValue(new Subject());
    const fixture = TestBed.createComponent(GithubAppComponent);
    fixture.detectChanges();
    const button: HTMLButtonElement = fixture.nativeElement.querySelector('[data-testid="github-app-create-button"]');
    button.click();
    fixture.detectChanges();
    button.click();
    expect(admin.requestGithubAppManifest).toHaveBeenCalledTimes(1);
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
    // Each credential is a copy field, so the value is the control's, not text.
    const values = (
      Array.from(fixture.nativeElement.querySelectorAll('gr-copy-field input, gr-copy-field textarea')) as
        (HTMLInputElement | HTMLTextAreaElement)[]
    ).map((el) => el.value);
    expect(values).toContain('PEM');
    expect(values).toContain('whsec');
    expect(values).toContain('cid');
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

  it('shows a spinner while credentials load and a failed load with Try Again', () => {
    setup({ ready: '1' });
    admin.fetchGithubAppCredentials.mockReturnValueOnce(throwError(() => ({ status: 500, message: 'database is down' })));
    const fixture = TestBed.createComponent(GithubAppComponent);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load settings for GitHub App');
    expect(root.textContent).toContain('database is down');
    admin.fetchGithubAppCredentials.mockReturnValue(new Subject());
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(admin.fetchGithubAppCredentials).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('Loading credentials...');
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

    function leaving(fixture: ComponentFixture<GithubAppComponent>): boolean[] {
      const answers: boolean[] = [];
      const result = fixture.componentInstance.canDeactivate();
      if (typeof result === 'boolean') answers.push(result);
      else result.subscribe((answer) => answers.push(answer));
      return answers;
    }

    it('allows leaving the setup view without prompting', () => {
      setup({});
      const fixture = TestBed.createComponent(GithubAppComponent);
      fixture.detectChanges();
      expect(leaving(fixture)).toEqual([true]);
      expect(confirm).not.toHaveBeenCalled();
    });

    it('prompts and blocks leaving when the user cancels', () => {
      const fixture = withCredentials();
      confirm.mockImplementation(() => EMPTY);
      expect(leaving(fixture)).toEqual([false]);
      expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'GitHub App credentials' }));
      expect(window.confirm).not.toHaveBeenCalled();
    });

    it('prompts and allows leaving when the user confirms', () => {
      const fixture = withCredentials();
      expect(leaving(fixture)).toEqual([true]);
      expect(confirm).toHaveBeenCalledOnce();
    });

    it('flags beforeunload while credentials are shown', () => {
      const fixture = withCredentials();
      const event = new Event('beforeunload', { cancelable: true });
      fixture.componentInstance.onBeforeUnload(event as BeforeUnloadEvent);
      expect(event.defaultPrevented).toBe(true);
    });
  });
});
