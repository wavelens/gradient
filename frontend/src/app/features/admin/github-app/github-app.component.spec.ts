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

describe('GithubAppComponent', () => {
  let admin: jasmine.SpyObj<AdminService>;

  function setup(queryParams: Record<string, string> = {}) {
    admin = jasmine.createSpyObj<AdminService>('AdminService', [
      'requestGithubAppManifest',
      'fetchGithubAppCredentials',
    ]);
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
    admin.requestGithubAppManifest.and.returnValue(
      of({ manifest: {}, post_url: 'https://github.com/x', state: 's' }),
    );
    const fixture = TestBed.createComponent(GithubAppComponent);
    fixture.componentInstance.host.set('ghe.example.com');
    fixture.componentInstance.create();
    expect(admin.requestGithubAppManifest).toHaveBeenCalledWith('ghe.example.com');
  });

  it('renders credentials when ready=1 and the API returns them', async () => {
    setup({ ready: '1' });
    admin.fetchGithubAppCredentials.and.returnValue(
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
    admin.fetchGithubAppCredentials.and.returnValue(
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
});
