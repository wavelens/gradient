/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, signal, computed, inject } from '@angular/core';
import { Router, UrlTree } from '@angular/router';
import { BehaviorSubject, Observable, filter, of, take } from 'rxjs';
import { tap, switchMap, finalize, catchError, map, shareReplay } from 'rxjs/operators';
import { ApiError, ApiService } from './api.service';
import { MakeUserRequest, UserInfoResponse } from '@core/models';
import { asSessionProbe } from '@core/interceptors/request-context';
import { links } from '@shared/routing';

const SIGNED_OUT_KEY = 'gradient-signed-out';

@Injectable({ providedIn: 'root' })
export class AuthService {
  private api = inject(ApiService);
  private router = inject(Router);

  // Signals for reactive state management
  private userSignal = signal<UserInfoResponse | null>(null);
  private tokenSignal = signal<string | null>(null);
  private loadingSignal = signal(false);
  private tokenRejectedOnStartup = false;

  // Emits true once the initial auth check is done (regardless of outcome)
  private initializedSubject = new BehaviorSubject(false);
  initialized$ = this.initializedSubject.asObservable().pipe(filter(v => v), take(1));

  // Computed signals (read-only)
  // True when the last probe never reached the server, so the session state is
  // unknown rather than absent.
  private unreachableSignal = signal(false);

  user = this.userSignal.asReadonly();
  token = this.tokenSignal.asReadonly();
  loading = this.loadingSignal.asReadonly();
  serverUnreachable = this.unreachableSignal.asReadonly();
  isAuthenticated = computed(() => !!this.userSignal());

  private sessionCheck?: Observable<boolean>;

  constructor() {
    this.initializeAuth();
    globalThis.addEventListener?.('storage', (event: StorageEvent) => {
      if (event.key === SIGNED_OUT_KEY && this.isAuthenticated()) {
        this.forget();
        this.toLogin(this.router.url);
      }
    });
  }

  /**
   * Initialize authentication state by probing the /user endpoint.
   * The JWT is stored in an httpOnly cookie so JS never touches it directly.
   */
  private initializeAuth(): void {
    this.loadUser(() => this.initializedSubject.next(true));
  }

  /**
   * Login with username/password
   */
  login(loginname: string, password: string, rememberMe: boolean) {
    this.loadingSignal.set(true);

    return this.api
      .post<string>('auth/basic/login', {
        loginname,
        password,
        remember_me: rememberMe,
      })
      .pipe(
        switchMap(() => this.api.get<UserInfoResponse>('user')),
        tap((user) => this.userSignal.set(user)),
        finalize(() => this.loadingSignal.set(false))
      );
  }

  /**
   * Register a new user
   */
  register(data: MakeUserRequest) {
    this.loadingSignal.set(true);

    return this.api
      .post('auth/basic/register', data)
      .pipe(finalize(() => this.loadingSignal.set(false)));
  }

  /**
   * Check if username is available
   */
  checkUsername(username: string) {
    return this.api.post<boolean>('auth/check-username', { username });
  }

  /**
   * Logout the current user
   */
  logout(returnUrl?: string) {
    return this.api.post('auth/logout', {}).pipe(
      finalize(() => {
        this.forget();
        localStorage.setItem(SIGNED_OUT_KEY, String(Date.now()));
        this.router.navigate(['/account/login'], returnUrl ? { queryParams: { next: returnUrl } } : {});
      })
    );
  }

  sessionLost(returnUrl: string): void {
    this.toLogin(returnUrl, this.clearStoredUserAndTokenAndReturnExpiredReason());
  }

  forgetRejectedSessionAndGetSignInPageUrl(returnUrl: string): UrlTree {
    return links.login(returnUrl, this.clearStoredUserAndTokenAndReturnExpiredReason());
  }

  private clearStoredUserAndTokenAndReturnExpiredReason(): 'expired' | undefined {
    const wasSignedIn = this.isAuthenticated();
    const expired = wasSignedIn || this.tokenRejectedOnStartup;
    this.forget();
    if (wasSignedIn) this.api.post('auth/logout', {}, asSessionProbe()).subscribe({ error: () => undefined });
    return expired ? 'expired' : undefined;
  }

  stillSignedIn(): Observable<boolean> {
    this.sessionCheck ??= this.api.get<UserInfoResponse>('user', asSessionProbe()).pipe(
      map(() => true),
      catchError((error: unknown) => of(error instanceof ApiError && error.unreachable)),
      finalize(() => (this.sessionCheck = undefined)),
      shareReplay(1),
    );
    return this.sessionCheck;
  }

  getSignInPageUrl(next: string): UrlTree {
    return links.login(next, this.tokenRejectedOnStartup ? 'expired' : undefined);
  }

  private forget(): void {
    this.tokenRejectedOnStartup = false;
    this.userSignal.set(null);
    this.tokenSignal.set(null);
  }

  private toLogin(returnUrl: string, reason?: 'expired'): void {
    const pending = this.router.currentNavigation()?.finalUrl;
    const target = pending ? this.router.serializeUrl(pending) : this.router.url;
    if (target.startsWith('/account/')) return;
    const carry = returnUrl && returnUrl !== '/' && !returnUrl.startsWith('/account/');
    const queryParams = { ...(carry ? { next: returnUrl } : {}), ...(reason ? { reason } : {}) };
    this.router.navigate(['/account/login'], Object.keys(queryParams).length ? { queryParams } : {});
  }

  /**
   * Load user information from API
   */
  private loadUser(onComplete?: () => void): void {
    this.probe().subscribe(() => onComplete?.());
  }

  /// Probes `/user` and records what it learned. An unreachable server says
  /// nothing about the session, so forgetting it here is what would turn an
  /// outage into a login screen; the state stays unknown until a real answer
  /// comes back.
  private probe(): Observable<boolean> {
    return this.api.get<UserInfoResponse>('user', asSessionProbe()).pipe(
      tap((user) => {
        this.userSignal.set(user);
        this.unreachableSignal.set(false);
        this.tokenRejectedOnStartup = false;
      }),
      map(() => true),
      catchError((error: unknown) => {
        const unreachable = error instanceof ApiError && error.unreachable;
        this.unreachableSignal.set(unreachable);
        this.tokenRejectedOnStartup = error instanceof ApiError && error.status === 401;
        if (!unreachable) {
          this.userSignal.set(null);
          this.tokenSignal.set(null);
        }
        return of(false);
      })
    );
  }

  /// Repeats the probe after an outage, so a guard does not decide on an answer
  /// that never arrived.
  resolveSession(): Observable<boolean> {
    if (!this.unreachableSignal()) {
      return of(this.isAuthenticated());
    }
    return this.probe();
  }

  /**
   * Complete login after an external flow (e.g. OIDC callback).
   * The cookie is already set by the backend redirect; just load the user.
   */
  loginWithCookie(): Observable<UserInfoResponse> {
    return this.api.get<UserInfoResponse>('user').pipe(
      tap((user) => this.userSignal.set(user))
    );
  }

  /**
   * Manually reload user data
   */
  reloadUser(): void {
    if (this.isAuthenticated()) {
      this.loadUser();
    }
  }
}
