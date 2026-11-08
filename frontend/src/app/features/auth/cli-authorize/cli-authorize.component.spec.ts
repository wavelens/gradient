/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { Subject, of, throwError } from 'rxjs';
import { ApiService } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { CliAuthorizeComponent, formatCliConfirmationCode } from './cli-authorize.component';

const INFO = { user_code: 'ABCD-EF23', ip: null, user_agent: null };

function setup(code: string) {
  const get = vi.fn(() => of(INFO));
  const post = vi.fn();
  TestBed.configureTestingModule({
    imports: [CliAuthorizeComponent],
    providers: [
      provideRouter([]),
      { provide: ApiService, useValue: { get, post } },
      { provide: AuthService, useValue: { initialized$: of(true), isAuthenticated: () => true } },
      { provide: ConfigService, useValue: { logoUrl: null } },
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

  it('sends one authorization for a double click and drops an earlier error on success', () => {
    const { fixture, post } = setup('ABCD-EF23');
    const component = fixture.componentInstance;
    post.mockReturnValueOnce(throwError(() => new Error('Transient failure')));
    component.authorize();
    expect(component.errorMessage()).toBe('Transient failure');
    const pending = new Subject<string>();
    post.mockReturnValueOnce(pending);
    component.authorize();
    component.authorize();
    pending.next('ok');
    expect(post).toHaveBeenCalledTimes(2);
    expect(component.errorMessage()).toBeNull();
    expect(component.status()).toBe('authorized');
  });

  it('returns to an empty code form for another code', () => {
    const { fixture } = setup('ABCD-EF23');
    const root = fixture.nativeElement as HTMLElement;
    Array.from(root.querySelectorAll('button')).find((b) => b.textContent?.includes('Enter another code'))!.click();
    fixture.detectChanges();
    expect(fixture.componentInstance.info()).toBeNull();
    expect(root.querySelector<HTMLInputElement>('#userCode')?.value).toBe('');
  });
});
