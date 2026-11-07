/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { firstValueFrom } from 'rxjs';
import { ApiError, ApiService, apiErrorMessage } from './api.service';

describe('ApiService errors', () => {
  function setup() {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    return { api: TestBed.inject(ApiService), http: TestBed.inject(HttpTestingController) };
  }

  async function failure(promise: Promise<unknown>): Promise<ApiError> {
    try {
      await promise;
    } catch (error) {
      return error as ApiError;
    }
    throw new Error('the request did not fail');
  }

  it('passes on the sentence from the server', async () => {
    const { api, http } = setup();
    const result = failure(firstValueFrom(api.delete('projects/acme')));
    http.expectOne(() => true).flush({ error: true, message: 'Project has running evaluations' }, { status: 409, statusText: 'Conflict' });
    const error = await result;
    expect(error).toBeInstanceOf(ApiError);
    expect(error.message).toBe('Project has running evaluations');
    expect(error.status).toBe(409);
  });

  it('never shows the transport text when the server gave no reason', async () => {
    const { api, http } = setup();
    const result = failure(firstValueFrom(api.post('evals/e1/retry')));
    http.expectOne(() => true).flush('', { status: 500, statusText: 'Internal Server Error' });
    const error = await result;
    expect(error.message).not.toContain('Http failure');
    expect(error.message).toBe('The server failed to process the request. Try again later.');
  });

  it('fails a request whose answer has no body, so the page shows an error instead of crashing', async () => {
    const { api, http } = setup();
    const result = failure(firstValueFrom(api.get('projects')));
    http.expectOne(() => true).flush(null, { status: 200, statusText: 'OK' });
    const error = await result;
    expect(error).toBeInstanceOf(ApiError);
    expect(error.message).toBe('The server returned an empty response. Try again later.');
  });

  it('accepts a No Content answer without a body', async () => {
    const { api, http } = setup();
    const result = firstValueFrom(api.delete('caches/c/nars/h'));
    http.expectOne(() => true).flush(null, { status: 204, statusText: 'No Content' });
    expect(await result).toBeUndefined();
  });

  it('reads the reason of a failed download from its JSON body', async () => {
    const { api, http } = setup();
    const result = failure(firstValueFrom(api.getBlob('evals/e1/report')));
    const body = new Blob([JSON.stringify({ error: true, message: 'Missing permission viewWorkers' })]);
    http.expectOne(() => true).flush(body, { status: 403, statusText: 'Forbidden' });
    const error = await result;
    expect(error).toBeInstanceOf(ApiError);
    expect(error.message).toBe('Missing permission viewWorkers');
  });

  it('falls back to the caller text for errors that did not come from the server', () => {
    expect(apiErrorMessage(new ApiError('Name taken', 409), 'Failed to save.')).toBe('Name taken');
    expect(apiErrorMessage(new TypeError('x is undefined'), 'Failed to save.')).toBe('Failed to save.');
  });
});
