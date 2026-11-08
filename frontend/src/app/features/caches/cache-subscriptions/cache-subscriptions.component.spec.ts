/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Observable, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { ApiError } from '@core/services/api.service';
import { CacheSubscriptionRequestsComponent } from './cache-subscriptions.component';
import { CachesService } from '@core/services/caches.service';
import { SubscriptionRequestItem } from '@core/models';
import { MessageService } from '@gradient/ui/ui';
import { cacheAccessData } from '@core/resolvers/access-route.testing';

const request: SubscriptionRequestItem = {
  project: 'wavelens',
  project_display_name: 'Wavelens',
  mode: 'ReadWrite',
  requested_by: 'alice',
  created_at: '2026-09-05T12:00:00',
};

function buttonByText(root: HTMLElement, text: string): HTMLButtonElement | undefined {
  return Array.from(root.querySelectorAll('button')).find((b) =>
    (b.textContent || '').trim().toLowerCase().includes(text.toLowerCase()),
  ) as HTMLButtonElement | undefined;
}

function setup(requests: SubscriptionRequestItem[], load?: () => Observable<SubscriptionRequestItem[]>, canManage = true) {
  const approve = vi.fn().mockReturnValue(of('Subscription approved'));
  const deny = vi.fn().mockReturnValue(of('Subscription request denied'));
  const confirm = vi.fn((confirmRequest: ConfirmRequest) => confirmRequest.action());

  TestBed.configureTestingModule({
    imports: [CacheSubscriptionRequestsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: { paramMap: convertToParamMap({ cache: 'prod' }) },
          parent: { data: of(cacheAccessData({ managed: false, canEdit: canManage, canTrigger: canManage })) },
        },
      },
      {
        provide: CachesService,
        useValue: {
          getCache: () => of({ display_name: 'Prod' }),
          getSubscriptionRequests: load ?? (() => of(requests)),
          approveSubscriptionRequest: approve,
          denySubscriptionRequest: deny,
        },
      },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });

  const fixture: ComponentFixture<CacheSubscriptionRequestsComponent> = TestBed.createComponent(
    CacheSubscriptionRequestsComponent,
  );
  fixture.detectChanges();
  return { fixture, approve, deny, confirm };
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

  it('shows a failed load as an error, never next to the empty state', () => {
    const load = vi.fn()
      .mockReturnValueOnce(throwError(() => new ApiError('Database is down', 500)))
      .mockReturnValue(of([request]));
    const { fixture } = setup([], load);
    const element = fixture.nativeElement as HTMLElement;
    expect(element.textContent).toContain('Failed to load subscription requests');
    expect(element.textContent).toContain('Database is down');
    expect(element.textContent).not.toContain('No pending requests');
    element.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(element.textContent).toContain('Wavelens');
    expect(element.textContent).not.toContain('Failed to load');
  });

  it('names the missing permission to a cache viewer and asks the server nothing', () => {
    const load = vi.fn(() => of([request]));
    const { fixture } = setup([], load, false);
    const element = fixture.nativeElement as HTMLElement;
    expect(load).not.toHaveBeenCalled();
    expect(element.textContent).toContain('No access');
    expect(element.textContent).toContain('manage cache subscriptions permission for this cache');
    expect(element.textContent).not.toContain('Try Again');
  });

  it('approves a request by project name', () => {
    const { fixture, approve } = setup([request]);
    buttonByText(fixture.nativeElement, 'Approve')?.click();
    expect(approve).toHaveBeenCalledWith('prod', 'wavelens');
  });

  it('asks before denying a request, naming the project, then denies it by project name', () => {
    const { fixture, deny, confirm } = setup([request]);
    buttonByText(fixture.nativeElement, 'Deny')?.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Wavelens' }));
    expect(deny).toHaveBeenCalledWith('prod', 'wavelens');
  });

  it('sends no denial when the confirmation is cancelled', () => {
    const { fixture, deny, confirm } = setup([request]);
    confirm.mockImplementation(() => EMPTY);
    buttonByText(fixture.nativeElement, 'Deny')?.click();
    expect(deny).not.toHaveBeenCalled();
  });

  it('shows a failed approve as an error toast with the server reason, not as raw text', () => {
    const { fixture, approve } = setup([request]);
    approve.mockReturnValue(throwError(() => new ApiError('Project wavelens was deleted', 404)));
    buttonByText(fixture.nativeElement, 'Approve')?.click();
    fixture.detectChanges();
    expect(TestBed.inject(MessageService).messages()).toContainEqual(
      expect.objectContaining({
        severity: 'error',
        summary: 'Failed to approve subscription request',
        detail: 'Project wavelens was deleted',
      }),
    );
    expect(fixture.componentInstance.deciding()).toBeNull();
    expect((fixture.nativeElement as HTMLElement).querySelector('gr-message-banner')).toBeNull();
  });
});
