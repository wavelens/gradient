/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { HttpClient, HttpHeaders } from '@angular/common/http';
import { Observable, throwError } from 'rxjs';
import { map, catchError } from 'rxjs/operators';
import { environment } from '@environments/environment';
import { ApiResponse } from '@core/models';

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
          if (response.error) {
            throw new Error(response.message as string);
          }
          return response.message as T;
        }),
        catchError((error) => {
          // Handle HTTP errors
          const errorMessage = error.error?.message || error.message || 'An unknown error occurred';
          return throwError(() => new Error(errorMessage));
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
