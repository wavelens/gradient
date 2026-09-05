/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { CacheSubscriptionRequestsComponent } from './cache-subscriptions.component';
import { CachesService } from '@core/services/caches.service';
import { SubscriptionRequest } from '@core/models';

const request: SubscriptionRequest = {
  project: 'wavelens',
  project_display_name: 'Wavelens',
  mode: 0,
  requested_by: 'alice',
  created_at: '2026-09-05T12:00:00',
};

function buttonByText(root: HTMLElement, text: string): HTMLButtonElement | undefined {
  return Array.from(root.querySelectorAll('button')).find((b) =>
    (b.textContent || '').trim().toLowerCase().includes(text.toLowerCase()),
  ) as HTMLButtonElement | undefined;
}

function setup(requests: SubscriptionRequest[]) {
  const approve = vi.fn().mockReturnValue(of('Subscription approved'));
  const deny = vi.fn().mockReturnValue(of('Subscription request denied'));

  TestBed.configureTestingModule({
    imports: [CacheSubscriptionRequestsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: ActivatedRoute,
        useValue: { snapshot: { paramMap: convertToParamMap({ cache: 'prod' }) } },
      },
      {
        provide: CachesService,
        useValue: {
          getSubscriptionRequests: () => of(requests),
          approveSubscriptionRequest: approve,
          denySubscriptionRequest: deny,
        },
      },
    ],
  });

  const fixture: ComponentFixture<CacheSubscriptionRequestsComponent> = TestBed.createComponent(
    CacheSubscriptionRequestsComponent,
  );
  fixture.detectChanges();
  return { fixture, approve, deny };
}

describe('CacheSubscriptionRequestsComponent', () => {
  it('lists a pending request with the asking project', () => {
    const { fixture } = setup([request]);
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Wavelens');
  });

  it('shows an empty state when nothing is waiting', () => {
    const { fixture } = setup([]);
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('No pending requests');
  });

  it('approves a request by project name', () => {
    const { fixture, approve } = setup([request]);
    buttonByText(fixture.nativeElement, 'Approve')?.click();
    expect(approve).toHaveBeenCalledWith('prod', 'wavelens');
  });

  it('denies a request by project name', () => {
    const { fixture, deny } = setup([request]);
    buttonByText(fixture.nativeElement, 'Deny')?.click();
    expect(deny).toHaveBeenCalledWith('prod', 'wavelens');
  });
});
