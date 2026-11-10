/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { HttpClient, HttpContext, HttpErrorResponse, HttpHeaders, HttpResponse } from '@angular/common/http';
import { Observable, from, throwError } from 'rxjs';
import { map, catchError, switchMap } from 'rxjs/operators';
import { environment } from '@environments/environment';
import { ApiResponse } from '@core/models';
import { plural } from '@shared/text';

/// A failed request, carrying the status so a caller can tell "the server said
/// no" from "the server never answered". Extends Error, so the many callers
/// that only read `message` keep working.
export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code?: string,
    readonly retryAfterSeconds?: number,
  ) {
    super(message);
    this.name = 'ApiError';
  }

  /// The server could not be reached or is not serving: nothing about the
  /// request itself was rejected.
  get unreachable(): boolean {
    return [0, 429, 502, 503, 504].includes(this.status);
  }
}

const NO_CONTENT = 204;
const EMPTY_RESPONSE = 'The server returned an empty response. Try again later.';

const SESSION_ENDED_ERROR_CODE = 'unauthorized';

const STATUS_MESSAGES: Record<number, string> = {
  0: 'Connection to the server failed. Check the connection and try again.',
  401: 'Your session has ended. Sign in again.',
  403: 'No permission for this action.',
  404: 'This item is already gone.',
  409: 'Other changes conflict with this one. Reload the page and try again.',
  429: 'The server is busy. Try again later.',
};

export function parseRetryAfterHeaderToMilliseconds(headers: HttpHeaders | undefined): number | undefined {
  const header = headers?.get('retry-after') ?? null;
  if (header === null) return undefined;
  const seconds = Number(header);
  const milliseconds = Number.isFinite(seconds) ? seconds * 1000 : Date.parse(header) - Date.now();
  return Number.isFinite(milliseconds) ? Math.max(0, milliseconds) : undefined;
}

function statusMessage(status: number, retryAfterSeconds?: number): string {
  if (status === 429 && retryAfterSeconds) return `Too many requests. Try again after ${plural(retryAfterSeconds, 'second')}.`;
  if (STATUS_MESSAGES[status]) return STATUS_MESSAGES[status];
  return status >= 500 ? 'The server failed to process the request. Try again later.' : 'The server rejected the request.';
}

function readErrorCode(body: unknown): string | undefined {
  const code = (body as { code?: unknown } | null)?.code;
  return typeof code === 'string' && code ? code : undefined;
}

function serverMessage(body: unknown): string | undefined {
  const message = (body as { message?: unknown } | null)?.message;
  return typeof message === 'string' && message ? message : undefined;
}

export function isSessionEndedUnauthorizedResponse(status: number, body: unknown): boolean {
  if (status !== 401) return false;
  const code = readErrorCode(body);
  return code === undefined || code === SESSION_ENDED_ERROR_CODE;
}

function toApiError(error: HttpErrorResponse, body: unknown = error.error): ApiError {
  const code = readErrorCode(body);
  const sessionEnded = isSessionEndedUnauthorizedResponse(error.status, body);
  const retryAfterMilliseconds = error.status === 429 ? parseRetryAfterHeaderToMilliseconds(error.headers) : undefined;
  const retryAfterSeconds = retryAfterMilliseconds === undefined ? undefined : Math.max(1, Math.ceil(retryAfterMilliseconds / 1000));
  return new ApiError((sessionEnded ? undefined : serverMessage(body)) ?? statusMessage(error.status, retryAfterSeconds), error.status, code, retryAfterSeconds);
}

function parseJson(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

export function apiErrorMessage(error: unknown, fallback: string): string {
  return error instanceof ApiError ? error.message : fallback;
}

export interface RequestOptions {
  headers?: HttpHeaders;
  context?: HttpContext;
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
    options?: RequestOptions
  ): Observable<T> {
    const url = `${this.baseUrl}/${endpoint}`;

    return this.http
      .request<ApiResponse<T>>(method, url, {
        body,
        ...options,
        observe: 'response',
      })
      .pipe(
        map((response) => {
          if (response.status === NO_CONTENT) return undefined as T;
          const envelope = response.body;
          if (envelope === null || envelope === undefined) {
            throw new ApiError(EMPTY_RESPONSE, response.status);
          }
          if (envelope.error) {
            throw new ApiError(serverMessage(envelope) ?? statusMessage(response.status), response.status, readErrorCode(envelope));
          }
          return envelope.message as T;
        }),
        catchError((error: unknown) => {
          if (error instanceof HttpErrorResponse) {
            return throwError(() => toApiError(error));
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
  get<T>(endpoint: string, options?: RequestOptions): Observable<T> {
    return this.request<T>('GET', endpoint, undefined, options);
  }

  /**
   * GET a binary body. Bypasses the JSON envelope unwrapping in `request`, and
   * keeps the full response so callers can read `Content-Disposition`.
   */
  getBlob(endpoint: string): Observable<HttpResponse<Blob>> {
    return this.http
      .get(`${this.baseUrl}/${endpoint}`, { observe: 'response', responseType: 'blob' })
      .pipe(
        catchError((error: unknown) => {
          if (!(error instanceof HttpErrorResponse)) return throwError(() => error);
          if (!(error.error instanceof Blob)) return throwError(() => toApiError(error));
          return from(error.error.text()).pipe(
            switchMap((text) => throwError(() => toApiError(error, parseJson(text)))),
          );
        }),
      );
  }

  /**
   * POST request
   */
  post<T>(endpoint: string, body?: unknown, options?: RequestOptions): Observable<T> {
    return this.request<T>('POST', endpoint, body, options);
  }

  /**
   * PUT request
   */
  put<T>(endpoint: string, body?: unknown, options?: RequestOptions): Observable<T> {
    return this.request<T>('PUT', endpoint, body, options);
  }

  /**
   * PATCH request
   */
  patch<T>(endpoint: string, body?: unknown, options?: RequestOptions): Observable<T> {
    return this.request<T>('PATCH', endpoint, body, options);
  }

  /**
   * DELETE request
   */
  delete<T>(endpoint: string, body?: unknown, options?: RequestOptions): Observable<T> {
    return this.request<T>('DELETE', endpoint, body, options);
  }
}
