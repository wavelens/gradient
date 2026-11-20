/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap } from '@angular/router';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Observable, Subject, of, throwError } from 'rxjs';
import type { Mock } from 'vitest';
import type { GitHostConnectionSummary } from '@core/models';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { GithubAppComponent } from './github-app.component';
import { AdminService } from '@core/services/admin.service';
import { ConfigService } from '@core/services/config.service';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';

type MockedAdmin = {
  requestGithubAppManifest: ReturnType<typeof vi.fn>;
  fetchGithubAppCredentials: ReturnType<typeof vi.fn>;
};

const installation: GitHostConnectionSummary = {
  id: 'c1',
  owner: { type: 'instance' },
  name: 'github-acme',
  display_name: 'GitHub',
  git_host_type: 'github',
  receives_events: true,
  reports_status: true,
  can_manage: true,
  account_login: 'acme',
  github_installation_id: 42,
  access_token_refreshes_automatically: false,
  refused_signature_count: 0,
  task_count: 1,
};

const credentials = {
  id: 7,
  slug: 'gradient',
  html_url: 'https://github.com/apps/gradient',
  pem: 'PEM',
  webhook_secret: 'whsec',
  client_id: 'cid',
  client_secret: 'csec',
};

describe('GithubAppComponent', () => {
  let admin: MockedAdmin;
  let notify: { success: ReturnType<typeof vi.fn>; failure: ReturnType<typeof vi.fn> };
  let confirm: Mock<(request: ConfirmRequest) => Observable<unknown>>;
  let connections: { list: ReturnType<typeof vi.fn>; listTasks: ReturnType<typeof vi.fn>; listDeliveries: ReturnType<typeof vi.fn> };

  function setup(queryParams: Record<string, string> = {}, app: { installUrl?: string | null } | null = null) {
    admin = {
      requestGithubAppManifest: vi.fn(),
      fetchGithubAppCredentials: vi.fn(),
    };
    notify = { success: vi.fn(), failure: vi.fn() };
    confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
    connections = { list: vi.fn(() => of([])), listTasks: vi.fn(() => of([])), listDeliveries: vi.fn(() => of([])) };
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
        { provide: GitHostConnectionsApi, useValue: connections },
        {
          provide: ActivatedRoute,
          useValue: {
            snapshot: { queryParamMap: convertToParamMap(queryParams) },
          },
        },
      ],
    });
    if (app) {
      const config = TestBed.inject(ConfigService);
      config.githubAppEnabled = true;
      config.githubAppPageUrl = 'https://github.com/apps/gradient-ci';
      config.githubAppInstallUrl = app.installUrl === undefined ? 'https://github.com/apps/gradient-ci/installations/new' : app.installUrl;
    }
  }

  function render(): { fixture: ComponentFixture<GithubAppComponent>; root: HTMLElement } {
    const fixture = TestBed.createComponent(GithubAppComponent);
    fixture.detectChanges();
    return { fixture, root: fixture.nativeElement as HTMLElement };
  }

  function button(root: HTMLElement, label: string): HTMLButtonElement | undefined {
    return Array.from(root.querySelectorAll<HTMLButtonElement>('button')).find((b) => b.textContent?.includes(label));
  }

  describe('without a GitHub App', () => {
    it('offers Create on GitHub as the primary action', () => {
      setup();
      const { root } = render();
      const create = root.querySelector('[data-testid="github-app-create-button"]');
      expect(create?.classList).toContain('gr-button--primary');
      expect(root.querySelector('[data-testid="github-app-install"]')).toBeNull();
    });

    it('creates the App under an organization or the personal account', () => {
      setup();
      admin.requestGithubAppManifest.mockReturnValue(EMPTY);
      const { fixture } = render();
      fixture.componentInstance.create();
      expect(admin.requestGithubAppManifest).toHaveBeenCalledWith(undefined, undefined);
      fixture.componentInstance.organization.set(' acme-corp ');
      fixture.componentInstance.create();
      expect(admin.requestGithubAppManifest).toHaveBeenLastCalledWith(undefined, 'acme-corp');
    });

    it('blocks Create for an organization name with other characters', () => {
      setup();
      const { fixture, root } = render();
      fixture.componentInstance.organization.set('acme/evil');
      fixture.detectChanges();
      expect(root.querySelector<HTMLButtonElement>('[data-testid="github-app-create-button"]')!.disabled).toBe(true);
    });

    it('keeps the GitHub Enterprise host under Advanced', () => {
      setup();
      admin.requestGithubAppManifest.mockReturnValue(EMPTY);
      const { fixture, root } = render();
      expect(root.querySelector('#github-host')).toBeNull();
      button(root, 'Advanced')!.click();
      fixture.detectChanges();
      expect(root.querySelector('#github-host')).not.toBeNull();
      fixture.componentInstance.host.set(' https://ghe.example.com/ ');
      fixture.detectChanges();
      fixture.componentInstance.create();
      expect(admin.requestGithubAppManifest).toHaveBeenCalledWith('ghe.example.com', undefined);
      fixture.componentInstance.host.set('not a host!!');
      fixture.detectChanges();
      expect(root.querySelector<HTMLButtonElement>('[data-testid="github-app-create-button"]')!.disabled).toBe(true);
      expect(root.textContent).toContain('Use only letters, numbers, dots and dashes');
    });

    it('reports a refused manifest request as a toast and enables the button again', () => {
      setup();
      const failure = new ApiError('GitHub App is already configured', 409);
      admin.requestGithubAppManifest.mockReturnValue(throwError(() => failure));
      const { fixture } = render();
      fixture.componentInstance.create();
      fixture.detectChanges();
      expect(notify.failure).toHaveBeenCalledWith('Setup for GitHub App did not start', failure);
      expect(fixture.nativeElement.querySelector('[data-testid="github-app-create-button"]').getAttribute('aria-busy')).toBeNull();
    });

    it('ignores a second click on Create on GitHub while the manifest request runs', () => {
      setup();
      admin.requestGithubAppManifest.mockReturnValue(new Subject());
      const { fixture } = render();
      const create: HTMLButtonElement = fixture.nativeElement.querySelector('[data-testid="github-app-create-button"]');
      create.click();
      fixture.detectChanges();
      create.click();
      expect(admin.requestGithubAppManifest).toHaveBeenCalledTimes(1);
    });
  });

  describe('with a GitHub App', () => {
    it('makes Install on GitHub the primary action and links the App', () => {
      setup({}, {});
      const { root } = render();
      expect(root.querySelector('[data-testid="github-app-install"]')?.getAttribute('href')).toBe('https://github.com/apps/gradient-ci/installations/new');
      expect(root.querySelector('[data-testid="github-app-page"]')?.getAttribute('href')).toBe('https://github.com/apps/gradient-ci');
      expect(root.querySelector('[data-testid="github-app-create-button"]')).toBeNull();
    });

    it('offers no install link when the server could not look up the App', () => {
      setup({}, { installUrl: null });
      const { root } = render();
      expect(root.querySelector('[data-testid="github-app-install"]')).toBeNull();
    });

    it('lists the installations of the App', () => {
      setup({}, {});
      connections.list.mockReturnValue(of([installation, { ...installation, id: 'c2', git_host_type: 'gitea', account_login: 'gitea-bot' }]));
      const { root } = render();
      expect(connections.list).toHaveBeenCalledWith({ type: 'instance' });
      const rows = Array.from(root.querySelectorAll('.connection-row'));
      expect(rows).toHaveLength(1);
      expect(rows[0].textContent).toContain('acme');
    });

    it('says when the App has no installation yet', () => {
      setup({}, {});
      const { root } = render();
      expect(root.querySelector('[data-testid="github-app-no-installations"]')).not.toBeNull();
    });

    it('replaces the App only after a confirmation', () => {
      setup({}, {});
      confirm.mockImplementation(() => EMPTY);
      const { fixture, root } = render();
      root.querySelector<HTMLButtonElement>('[data-testid="github-app-replace"]')!.click();
      fixture.detectChanges();
      expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Replace GitHub App' }));
      expect(root.querySelector('[data-testid="github-app-create-button"]')).toBeNull();

      confirm.mockImplementation((request) => request.action());
      root.querySelector<HTMLButtonElement>('[data-testid="github-app-replace"]')!.click();
      fixture.detectChanges();
      expect(root.querySelector('[data-testid="github-app-create-button"]')).not.toBeNull();
      button(root, 'Cancel')!.click();
      fixture.detectChanges();
      expect(root.querySelector('[data-testid="github-app-create-button"]')).toBeNull();
    });
  });

  describe('after GitHub returns from an install', () => {
    afterEach(() => vi.useRealTimers());

    it('shows the new installation and the tasks it linked', () => {
      setup({ installation_id: '42', setup_action: 'install' }, {});
      connections.list.mockReturnValue(of([installation]));
      connections.listTasks.mockReturnValue(of([{ id: 't1', project: 'acme', name: 'app', display_name: 'App', repository: 'https://github.com/acme/app', report_status: true }]));
      const { root } = render();
      const returned = root.querySelector('[data-testid="github-app-returned"]')!;
      expect(returned.textContent).toContain('Installed on acme');
      expect(returned.querySelector('a')?.textContent).toContain('acme / App');
      expect(connections.listTasks).toHaveBeenCalledWith({ type: 'instance' }, 'c1');
      expect(root.querySelector('#connection-c1')?.classList).toContain('connection-row-linked');
    });

    it('waits for the installation event from GitHub', () => {
      vi.useFakeTimers();
      setup({ installation_id: '42', setup_action: 'install' }, {});
      const { fixture, root } = render();
      expect(root.querySelector('[data-testid="github-app-returned"]')?.textContent).toContain('Waiting for GitHub');
      connections.list.mockReturnValue(of([installation]));
      vi.advanceTimersByTime(2000);
      fixture.detectChanges();
      expect(root.querySelector('[data-testid="github-app-returned"]')?.textContent).toContain('Installed on acme');
    });

    it('offers to check again when GitHub never reports the installation', () => {
      vi.useFakeTimers();
      setup({ installation_id: '42', setup_action: 'install' }, {});
      const { fixture, root } = render();
      vi.advanceTimersByTime(60_000);
      fixture.detectChanges();
      const returned = root.querySelector('[data-testid="github-app-returned"]')!;
      expect(returned.textContent).not.toContain('Waiting for GitHub');
      connections.list.mockReturnValue(of([installation]));
      button(root, 'Check again')!.click();
      fixture.detectChanges();
      expect(root.querySelector('[data-testid="github-app-returned"]')?.textContent).toContain('Installed on acme');
    });

    it('tells that an owner of the account has to approve a requested install', () => {
      setup({ setup_action: 'request' }, {});
      const { root } = render();
      expect(root.querySelector('[data-testid="github-app-returned"]')?.textContent).toContain('approve');
    });
  });

  describe('after GitHub created the App', () => {
    it('shows only the values the server configuration uses', async () => {
      setup({ ready: '1' });
      admin.fetchGithubAppCredentials.mockReturnValue(of(credentials));
      const { fixture, root } = render();
      await fixture.whenStable();
      fixture.detectChanges();
      const values = (
        Array.from(root.querySelectorAll('gr-copy-field input, gr-copy-field textarea')) as (HTMLInputElement | HTMLTextAreaElement)[]
      ).map((el) => el.value);
      expect(values).toEqual(['7', 'PEM', 'whsec']);
      expect(root.querySelector('[data-testid="github-app-page"]')?.getAttribute('href')).toBe('https://github.com/apps/gradient');
      expect(root.querySelector('gr-message-banner')?.textContent).toContain('show only once');
      expect(root.querySelector('pre')?.textContent).toContain('id = 7;');
    });

    it('shows a friendly error when credentials are no longer available', async () => {
      setup({ ready: '1' });
      admin.fetchGithubAppCredentials.mockReturnValue(throwError(() => ({ status: 404 })));
      const { fixture, root } = render();
      await fixture.whenStable();
      fixture.detectChanges();
      expect(root.querySelector('[data-testid="github-app-credentials-missing"]')).toBeTruthy();

      const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
      button(root, 'Start again')!.click();
      fixture.detectChanges();
      expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { ready: null } }));
      expect(root.querySelector('[data-testid="github-app-create-button"]')).toBeTruthy();
    });

    it('shows a spinner while credentials load and a failed load with Try Again', () => {
      setup({ ready: '1' });
      admin.fetchGithubAppCredentials.mockReturnValueOnce(throwError(() => ({ status: 500, message: 'database is down' })));
      const { fixture, root } = render();
      expect(root.textContent).toContain('Failed to load settings for GitHub App');
      expect(root.textContent).toContain('database is down');
      admin.fetchGithubAppCredentials.mockReturnValue(new Subject());
      root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
      fixture.detectChanges();
      expect(admin.fetchGithubAppCredentials).toHaveBeenCalledTimes(2);
      expect(root.textContent).toContain('Loading credentials...');
    });
  });

  describe('navigation guard while credentials are shown', () => {
    function withCredentials() {
      setup({ ready: '1' });
      admin.fetchGithubAppCredentials.mockReturnValue(of(credentials));
      return render().fixture;
    }

    function leaving(fixture: ComponentFixture<GithubAppComponent>): boolean[] {
      const answers: boolean[] = [];
      const result = fixture.componentInstance.canDeactivate();
      if (typeof result === 'boolean') answers.push(result);
      else result.subscribe((answer) => answers.push(answer));
      return answers;
    }

    it('allows leaving the setup view without prompting', () => {
      setup();
      expect(leaving(render().fixture)).toEqual([true]);
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
