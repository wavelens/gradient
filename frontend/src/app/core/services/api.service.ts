/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { HttpClient, HttpErrorResponse, HttpHeaders, HttpResponse } from '@angular/common/http';
import { Observable, throwError } from 'rxjs';
import { map, catchError } from 'rxjs/operators';
import { environment } from '@environments/environment';
import { ApiResponse } from '@core/models';

/// A failed request, carrying the status so a caller can tell "the server said
/// no" from "the server never answered". Extends Error, so the many callers
/// that only read `message` keep working.
export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = 'ApiError';
  }

  /// The server could not be reached or is not serving: nothing about the
  /// request itself was rejected.
  get unreachable(): boolean {
    return this.status === 0 || this.status === 502 || this.status === 503 || this.status === 504;
  }
}

@Injectable({ providedIn: 'root' })
export class ApiService {
  private http = inject(HttpClient);
  private baseUrl = environment.apiUrl;

  /**
   * Generic HTTP request wrapper that handles API response unwrapping
   */
  private request<T>(
    method: string,
    endpoint: string,
    body?: unknown,
    options?: { headers?: HttpHeaders }
  ): Observable<T> {
    const url = `${this.baseUrl}/${endpoint}`;

    return this.http
      .request<ApiResponse<T>>(method, url, {
        body,
        ...options,
      })
      .pipe(
        map((response) => {
          if (response === null || response === undefined) {
            return undefined as T;
          }
          if (response.error) {
            throw new Error(response.message as string);
          }
          return response.message as T;
        }),
        catchError((error: unknown) => {
          if (error instanceof HttpErrorResponse) {
            const message = error.error?.message || error.message || 'An unknown error occurred';
            return throwError(() => new ApiError(message, error.status));
          }
          return throwError(() =>
            error instanceof Error ? error : new Error('An unknown error occurred')
          );
        })
      );
  }

  /**
   * GET request
   */
  get<T>(endpoint: string, options?: { headers?: HttpHeaders }): Observable<T> {
    return this.request<T>('GET', endpoint, undefined, options);
  }

  /**
   * GET a binary body. Bypasses the JSON envelope unwrapping in `request`, and
   * keeps the full response so callers can read `Content-Disposition`.
   */
  getBlob(endpoint: string): Observable<HttpResponse<Blob>> {
    return this.http.get(`${this.baseUrl}/${endpoint}`, {
      observe: 'response',
      responseType: 'blob',
    });
  }

  /**
   * POST request
   */
  post<T>(endpoint: string, body?: unknown, options?: { headers?: HttpHeaders }): Observable<T> {
    return this.request<T>('POST', endpoint, body, options);
  }

  /**
   * PUT request
   */
  put<T>(endpoint: string, body?: unknown, options?: { headers?: HttpHeaders }): Observable<T> {
    return this.request<T>('PUT', endpoint, body, options);
  }

  /**
   * PATCH request
   */
  patch<T>(endpoint: string, body?: unknown, options?: { headers?: HttpHeaders }): Observable<T> {
    return this.request<T>('PATCH', endpoint, body, options);
  }

  /**
   * DELETE request
   */
  delete<T>(endpoint: string, body?: unknown, options?: { headers?: HttpHeaders }): Observable<T> {
    return this.request<T>('DELETE', endpoint, body, options);
  }
}
