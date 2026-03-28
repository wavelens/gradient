/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, signal, computed, inject } from '@angular/core';
import { Router } from '@angular/router';
import { BehaviorSubject, Observable, of, filter, take } from 'rxjs';
import { tap, map, switchMap, catchError, finalize } from 'rxjs/operators';
import { ApiService } from './api.service';
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
  user = this.userSignal.asReadonly();
  token = this.tokenSignal.asReadonly();
  loading = this.loadingSignal.asReadonly();
  isAuthenticated = computed(() => !!this.userSignal());

  constructor() {
    // Restore session from storage on initialization
    this.initializeAuth();
  }

  /**
   * Initialize authentication state from stored token
   */
  private initializeAuth(): void {
    const storedToken = localStorage.getItem('jwt_token');

    if (storedToken) {
      this.tokenSignal.set(storedToken);
      this.loadUser(() => this.initializedSubject.next(true));
    } else {
      this.initializedSubject.next(true);
    }
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
        tap((token) => {
          this.tokenSignal.set(token);
          localStorage.setItem('jwt_token', token);
        }),
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
        localStorage.removeItem('jwt_token');
        this.router.navigate(['/account/login']);
      })
    );
  }

  /**
   * Load user information from API
   */
  private loadUser(onComplete?: () => void): void {
    this.api.get<User>('user').subscribe({
      next: (user) => {
        this.userSignal.set(user);
        onComplete?.();
      },
      error: () => {
        this.userSignal.set(null);
        this.tokenSignal.set(null);
        localStorage.removeItem('jwt_token');
        onComplete?.();
      },
    });
  }

  /**
   * Get the current JWT token
   */
  getToken(): string | null {
    return this.tokenSignal();
  }

  /**
   * Complete login with a token received from an external flow (e.g. OIDC callback)
   */
  loginWithToken(token: string): Observable<User> {
    this.tokenSignal.set(token);
    localStorage.setItem('jwt_token', token);
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
