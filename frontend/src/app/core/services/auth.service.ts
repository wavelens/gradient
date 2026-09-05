/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, signal, computed, inject } from '@angular/core';
import { Router } from '@angular/router';
import { BehaviorSubject, Observable, filter, of, take } from 'rxjs';
import { tap, switchMap, finalize, catchError, map } from 'rxjs/operators';
import { ApiError, ApiService } from './api.service';
import { User } from '@core/models';

@Injectable({ providedIn: 'root' })
export class AuthService {
  private api = inject(ApiService);
  private router = inject(Router);

  // Signals for reactive state management
  private userSignal = signal<User | null>(null);
  private tokenSignal = signal<string | null>(null);
  private loadingSignal = signal(false);

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

  constructor() {
    // Restore session from storage on initialization
    this.initializeAuth();
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
        switchMap(() => this.api.get<User>('user')),
        tap((user) => this.userSignal.set(user)),
        finalize(() => this.loadingSignal.set(false))
      );
  }

  /**
   * Register a new user
   */
  register(data: {
    username: string;
    name: string;
    email: string;
    password: string;
  }) {
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
  logout() {
    return this.api.post('auth/logout', {}).pipe(
      finalize(() => {
        this.userSignal.set(null);
        this.tokenSignal.set(null);
        this.router.navigate(['/account/login']);
      })
    );
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
    return this.api.get<User>('user').pipe(
      tap((user) => {
        this.userSignal.set(user);
        this.unreachableSignal.set(false);
      }),
      map(() => true),
      catchError((error: unknown) => {
        const unreachable = error instanceof ApiError && error.unreachable;
        this.unreachableSignal.set(unreachable);
        if (!unreachable) {
          this.userSignal.set(null);
          this.tokenSignal.set(null);
        }
        return of(false);
      })
    );
  }

  /// Re-runs the probe after an outage, so a guard does not decide on an answer
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
  loginWithCookie(): Observable<User> {
    return this.api.get<User>('user').pipe(
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
