/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { Subject, of, throwError } from 'rxjs';
import { ApiError, ApiService } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { CurrentTimeSignal } from '@shared/text';
import { CliAuthorizeComponent, formatCliConfirmationCode } from './cli-authorize.component';

const NOW = Date.parse('2026-11-08T12:00:00Z');
const INFO = { user_code: 'ABCD-EF23', ip: null, user_agent: null, expires_at: '2026-11-08T12:09:05' };

function setup(code: string, now = NOW) {
  const get = vi.fn(() => of(INFO));
  const post = vi.fn();
  TestBed.configureTestingModule({
    imports: [CliAuthorizeComponent],
    providers: [
      provideRouter([]),
      { provide: ApiService, useValue: { get, post } },
      { provide: AuthService, useValue: { initialized$: of(true), isAuthenticated: () => true, stillSignedIn: () => of(true) } },
      { provide: ConfigService, useValue: { logoUrl: null } },
      { provide: CurrentTimeSignal, useValue: { currentTimeMilliseconds: signal(now) } },
      { provide: ActivatedRoute, useValue: { snapshot: { queryParamMap: convertToParamMap({ code }) } } },
    ],
  });
  const fixture = TestBed.createComponent(CliAuthorizeComponent);
  fixture.detectChanges();
  return { fixture, get, post };
}

describe('CliAuthorizeComponent', () => {
  it('writes a typed code in the form the server issued', () => {
    expect(formatCliConfirmationCode(' abcd ef23 ')).toBe('ABCD-EF23');
    expect(formatCliConfirmationCode('abcdef23')).toBe('ABCD-EF23');
    expect(formatCliConfirmationCode('abc')).toBe('ABC');
  });

  it('looks up the code from the link in the server format', () => {
    const { get } = setup('abcdef23');
    expect(get).toHaveBeenCalledWith('auth/cli/info?user_code=ABCD-EF23');
  });

  it('returns to the code form with the server reason when authorization fails', () => {
    const { fixture, post } = setup('ABCD-EF23');
    const component = fixture.componentInstance;
    post.mockReturnValueOnce(throwError(() => new ApiError('Authorization expired', 410)));
    component.authorize();
    expect(component.errorMessage()).toBe("Couldn't approve CLI session: Authorization expired");
    expect(component.info()).toBeNull();
    expect(component.submitting()).toBe(false);
  });

  it('sends one authorization for a double click', () => {
    const { fixture, post } = setup('ABCD-EF23');
    const component = fixture.componentInstance;
    const pending = new Subject<string>();
    post.mockReturnValueOnce(pending);
    component.authorize();
    component.authorize();
    pending.next('ok');
    expect(post).toHaveBeenCalledTimes(1);
    expect(component.errorMessage()).toBeNull();
    expect(component.status()).toBe('authorized');
  });

  it('counts down to the code expiry and returns to the code form once it passed', () => {
    const root = (fixture: ReturnType<typeof setup>['fixture']) => fixture.nativeElement as HTMLElement;
    const buttonLabels = (fixture: ReturnType<typeof setup>['fixture']) => Array.from(root(fixture).querySelectorAll('button')).map((b) => b.textContent?.trim());

    const pending = setup('ABCD-EF23').fixture;
    expect(root(pending).querySelector('[data-testid="cli-code-expiration"]')?.textContent?.trim()).toBe('9:05');
    expect(root(pending).querySelector('[data-testid="cli-session-grant"]')?.textContent).toContain('full access');
    expect(buttonLabels(pending)).toEqual(expect.arrayContaining(['Authorize', 'Deny']));

    TestBed.resetTestingModule();
    const expired = setup('ABCD-EF23', Date.parse('2026-11-08T12:10:00Z')).fixture;
    expired.detectChanges();
    expect(expired.componentInstance.info()).toBeNull();
    expect(root(expired).textContent).toContain('The code has expired. Run gradient login again.');
    expect(buttonLabels(expired)).not.toContain('Deny');
  });

  it('names an unknown code instead of showing the raw server reason', () => {
    const { fixture, get } = setup('ABCD-EF23');
    get.mockReturnValueOnce(throwError(() => new ApiError('Device authorization not found', 404)));
    fixture.componentInstance.clearCodeLookupAndShowCodeForm();
    fixture.componentInstance.form.patchValue({ userCode: 'wxyz2345' });
    fixture.componentInstance.onLookup();
    expect(fixture.componentInstance.errorMessage()).toBe('Unknown or expired code. Run gradient login again.');
  });

  it('writes the typed code into the address so a reload keeps it', () => {
    const { fixture } = setup('');
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    fixture.componentInstance.form.patchValue({ userCode: 'wxyz2345' });
    fixture.componentInstance.onLookup();
    expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { code: 'WXYZ-2345' }, replaceUrl: true }));
  });

  it('returns to an empty code form for another code', () => {
    const { fixture } = setup('ABCD-EF23');
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    const root = fixture.nativeElement as HTMLElement;
    Array.from(root.querySelectorAll('button')).find((b) => b.textContent?.includes('Enter another code'))!.click();
    fixture.detectChanges();
    expect(fixture.componentInstance.info()).toBeNull();
    expect(root.querySelector<HTMLInputElement>('#userCode')?.value).toBe('');
    expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { code: null }, replaceUrl: true }));
  });
});
