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
import { MessageService, SelectComponent } from '@gradient/ui/ui';
import { By } from '@angular/platform-browser';
import { cacheAccessData } from '@core/resolvers/access-route.testing';

const request: SubscriptionRequestItem = {
  cache: 'prod',
  cache_display_name: 'Prod',
  project: 'wavelens',
  project_display_name: 'Wavelens',
  mode: 'ReadWrite',
  requested_by: 'alice',
  created_at: '2026-09-05T12:00:00',
};
const readOnlyRequest: SubscriptionRequestItem = { ...request, mode: 'ReadOnly' };

function buttonByText(root: HTMLElement, text: string): HTMLButtonElement | undefined {
  return Array.from(root.querySelectorAll('button')).find((b) =>
    (b.textContent || '').trim().toLowerCase().includes(text.toLowerCase()),
  ) as HTMLButtonElement | undefined;
}

function approveDialog(): HTMLElement {
  return document.querySelector('.gr-dialog') as HTMLElement;
}

function offeredModes(fixture: ComponentFixture<CacheSubscriptionRequestsComponent>): unknown[] {
  const select = fixture.debugElement.query(By.directive(SelectComponent)).componentInstance as SelectComponent;
  return select.options().map((option) => (option as { value: unknown }).value);
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

  it('names the requester by display name, falling back to the username', () => {
    const { fixture } = setup([{ ...request, requested_by_display_name: 'Alice Doe' }, { ...request, project: 'docs' }]);
    const meta = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('[slot="meta"]')).map((m) => m.textContent ?? '');
    expect(meta[0]).toContain('Requested by Alice Doe');
    expect(meta[1]).toContain('Requested by alice');
  });

  it('shows the requested access mode so push access stands out from read-only', () => {
    const { fixture } = setup([request, { ...request, project: 'docs', project_display_name: 'Docs', mode: 'ReadOnly' }]);
    const badges = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('gr-badge')).map((b) => b.textContent?.trim());
    expect(badges).toEqual(['Read & Write', 'Read Only']);
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
    expect(element.textContent).toContain('Only the admins of this cache can view this.');
    expect(element.textContent).not.toContain('Try Again');
  });

  it('approves a read-only request by project name without asking', () => {
    const { fixture, approve, confirm } = setup([readOnlyRequest]);
    buttonByText(fixture.nativeElement, 'Approve')?.click();
    expect(confirm).not.toHaveBeenCalled();
    expect(approve).toHaveBeenCalledWith('prod', 'wavelens');
  });

  it('opens an approve dialog for a request that may push, showing the requested mode', () => {
    const { fixture, approve, confirm } = setup([request]);
    buttonByText(fixture.nativeElement, 'Approve')?.click();
    fixture.detectChanges();
    const dialog = approveDialog();
    expect(dialog.textContent).toContain('Wavelens');
    expect(dialog.textContent).toContain('Requested mode: Read & Write');
    expect(dialog.textContent).toContain('The project can push store paths to this cache.');
    expect(confirm).not.toHaveBeenCalled();
    expect(approve).not.toHaveBeenCalled();
  });

  it('lets the approver lower a read and write request to any mode and sends the chosen one', () => {
    const { fixture, approve } = setup([request]);
    buttonByText(fixture.nativeElement, 'Approve')?.click();
    fixture.detectChanges();
    expect(offeredModes(fixture)).toEqual(['ReadWrite', 'ReadOnly', 'WriteOnly']);
    fixture.componentInstance.approvedSubscriptionMode.set('ReadOnly');
    fixture.detectChanges();
    expect(approveDialog().textContent).not.toContain('can push store paths');
    buttonByText(approveDialog(), 'Approve')!.click();
    expect(approve).toHaveBeenCalledWith('prod', 'wavelens', 'ReadOnly');
    expect(fixture.componentInstance.subscriptionRequestInApproveDialog()).toBeNull();
  });

  it('offers a write only request no other mode', () => {
    const { fixture, approve } = setup([{ ...request, mode: 'WriteOnly' }]);
    buttonByText(fixture.nativeElement, 'Approve')?.click();
    fixture.detectChanges();
    expect(offeredModes(fixture)).toEqual(['WriteOnly']);
    buttonByText(approveDialog(), 'Approve')!.click();
    expect(approve).toHaveBeenCalledWith('prod', 'wavelens', 'WriteOnly');
  });

  it('keeps a failed approval inside the open dialog', () => {
    const { fixture, approve } = setup([request]);
    approve.mockReturnValue(throwError(() => new ApiError('Project wavelens was deleted', 404)));
    buttonByText(fixture.nativeElement, 'Approve')?.click();
    fixture.detectChanges();
    buttonByText(approveDialog(), 'Approve')!.click();
    fixture.detectChanges();
    expect(approveDialog().querySelector('gr-message-banner')?.textContent).toContain('Project wavelens was deleted');
    expect(fixture.componentInstance.subscriptionRequestInApproveDialog()).not.toBeNull();
  });

  it('sends no approval when the approve dialog is cancelled', () => {
    const { fixture, approve } = setup([request]);
    buttonByText(fixture.nativeElement, 'Approve')?.click();
    fixture.detectChanges();
    buttonByText(approveDialog(), 'Cancel')!.click();
    fixture.detectChanges();
    expect(approve).not.toHaveBeenCalled();
    expect(fixture.componentInstance.subscriptionRequestInApproveDialog()).toBeNull();
  });

  it('asks before denying a request, naming the project, then denies it by project name', () => {
    const { fixture, deny, confirm } = setup([request]);
    buttonByText(fixture.nativeElement, 'Deny')?.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Wavelens (wavelens)' }));
    expect(deny).toHaveBeenCalledWith('prod', 'wavelens');
  });

  it('sends no denial when the confirmation is cancelled', () => {
    const { fixture, deny, confirm } = setup([request]);
    confirm.mockImplementation(() => EMPTY);
    buttonByText(fixture.nativeElement, 'Deny')?.click();
    expect(deny).not.toHaveBeenCalled();
  });

  it('shows a failed approve as an error toast with the server reason, not as raw text', () => {
    const { fixture, approve } = setup([readOnlyRequest]);
    approve.mockReturnValue(throwError(() => new ApiError('Project wavelens was deleted', 404)));
    buttonByText(fixture.nativeElement, 'Approve')?.click();
    fixture.detectChanges();
    expect(TestBed.inject(MessageService).messages()).toContainEqual(
      expect.objectContaining({
        severity: 'error',
        summary: "Couldn't approve the subscription request",
        detail: 'Project wavelens was deleted',
      }),
    );
    expect(fixture.componentInstance.deciding()).toBeNull();
    expect((fixture.nativeElement as HTMLElement).querySelector('gr-message-banner')).toBeNull();
  });
});
