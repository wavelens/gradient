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

type Fixture = ReturnType<typeof TestBed.createComponent<CliAuthorizeComponent>>;

function typeCodeAndPressLookUpButton(fixture: Fixture, typed: string) {
  fixture.componentInstance.form.patchValue({ userCode: typed });
  fixture.componentInstance.onLookup();
  fixture.detectChanges();
}

function setup(code: string | null, now = NOW, typed: string | null = 'abcdef23') {
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
      { provide: ActivatedRoute, useValue: { snapshot: { queryParamMap: convertToParamMap(code === null ? {} : { code }) } } },
    ],
  });
  const router = TestBed.inject(Router);
  const navigate = vi.spyOn(router, 'navigate').mockResolvedValue(true);
  const navigateByUrl = vi.spyOn(router, 'navigateByUrl').mockResolvedValue(true);
  const fixture = TestBed.createComponent(CliAuthorizeComponent);
  fixture.detectChanges();
  if (typed !== null) typeCodeAndPressLookUpButton(fixture, typed);
  return { fixture, get, post, navigate, navigateByUrl };
}

describe('CliAuthorizeComponent', () => {
  it('writes a typed code in the form the server issued', () => {
    expect(formatCliConfirmationCode(' abcd ef23 ')).toBe('ABCD-EF23');
    expect(formatCliConfirmationCode('abcdef23')).toBe('ABCD-EF23');
    expect(formatCliConfirmationCode('abc')).toBe('ABC');
  });

  it('Ignore the code in the link. Keep the field empty. Send no query. Discard the code from the address.', () => {
    const { fixture, get, navigateByUrl } = setup('ABCD-EFGH', NOW, null);
    expect(fixture.componentInstance.form.value.userCode).toBe('');
    expect((fixture.nativeElement as HTMLElement).querySelector<HTMLInputElement>('#userCode')?.value).toBe('');
    expect(fixture.componentInstance.info()).toBeNull();
    expect(get).not.toHaveBeenCalled();
    expect(navigateByUrl).toHaveBeenCalledWith(expect.anything(), { replaceUrl: true });
    expect(String(navigateByUrl.mock.calls[0][0])).not.toContain('code');
  });

  it('Query the entered code in server format and approve it', () => {
    const { fixture, get, post } = setup(null);
    expect(get).toHaveBeenCalledWith('auth/cli/info?user_code=ABCD-EF23');
    post.mockReturnValueOnce(of(null));
    fixture.componentInstance.authorize();
    expect(post).toHaveBeenCalledWith('auth/cli/authorize', { user_code: 'ABCD-EF23' });
    expect(fixture.componentInstance.status()).toBe('authorized');
  });

  it('returns to the code form with the server reason when authorization fails', () => {
    const { fixture, post } = setup(null);
    const component = fixture.componentInstance;
    post.mockReturnValueOnce(throwError(() => new ApiError('Authorization expired', 410)));
    component.authorize();
    expect(component.errorMessage()).toBe("Couldn't approve CLI session: Authorization expired");
    expect(component.info()).toBeNull();
    expect(component.submitting()).toBe(false);
  });

  it('sends one authorization for a double click', () => {
    const { fixture, post } = setup(null);
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

    const pending = setup(null).fixture;
    expect(root(pending).querySelector('[data-testid="cli-code-expiration"]')?.textContent?.trim()).toBe('9:05');
    expect(root(pending).querySelector('[data-testid="cli-session-grant"]')?.textContent).toContain('full access');
    expect(buttonLabels(pending)).toEqual(expect.arrayContaining(['Authorize', 'Deny']));

    TestBed.resetTestingModule();
    const expired = setup(null, Date.parse('2026-11-08T12:10:00Z')).fixture;
    expired.detectChanges();
    expect(expired.componentInstance.info()).toBeNull();
    expect(root(expired).textContent).toContain('The code has expired. Run gradient login again.');
    expect(buttonLabels(expired)).not.toContain('Deny');
  });

  it('names an unknown code instead of showing the raw server reason', () => {
    const { fixture, get } = setup(null);
    get.mockReturnValueOnce(throwError(() => new ApiError('Device authorization not found', 404)));
    fixture.componentInstance.clearCodeLookupAndShowCodeForm();
    fixture.componentInstance.form.patchValue({ userCode: 'wxyz2345' });
    fixture.componentInstance.onLookup();
    expect(fixture.componentInstance.errorMessage()).toBe('Unknown or expired code. Run gradient login again.');
  });

  it('Does not put the entered code in the URL', () => {
    const { navigate, navigateByUrl } = setup(null);
    expect(navigate).not.toHaveBeenCalled();
    expect(navigateByUrl).not.toHaveBeenCalled();
  });

  it('returns to an empty code form for another code', () => {
    const { fixture } = setup(null);
    const root = fixture.nativeElement as HTMLElement;
    Array.from(root.querySelectorAll('button')).find((b) => b.textContent?.includes('Enter another code'))!.click();
    fixture.detectChanges();
    expect(fixture.componentInstance.info()).toBeNull();
    expect(root.querySelector<HTMLInputElement>('#userCode')?.value).toBe('');
  });
});
