/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { EvaluationsService } from './evaluations.service';
import { environment } from '@environments/environment';

describe('EvaluationsService prioritize', () => {
  let service: EvaluationsService;
  let httpMock: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [EvaluationsService, provideHttpClient(), provideHttpClientTesting()],
    });
    service = TestBed.inject(EvaluationsService);
    httpMock = TestBed.inject(HttpTestingController);
  });

  afterEach(() => httpMock.verify());

  it('prioritizes an evaluation with a bodyless POST', () => {
    let result: string | undefined;
    service.prioritizeEvaluation('970223d8-4c52-46d7-b8fd-0d0a900facb3').subscribe((r) => (result = r));
    const request = httpMock.expectOne(`${environment.apiUrl}/evals/970223d8-4c52-46d7-b8fd-0d0a900facb3/prioritize`);
    expect(request.request.method).toBe('POST');
    expect(request.request.body).toBeNull();
    request.flush({ error: false, message: 'Success' });
    expect(result).toBe('Success');
  });

  it('prioritizes a build with a bodyless POST', () => {
    let result: string | undefined;
    service.prioritizeBuild('01a05a38-3276-7252-bc05-c139d9c8a015').subscribe((r) => (result = r));
    const request = httpMock.expectOne(`${environment.apiUrl}/builds/01a05a38-3276-7252-bc05-c139d9c8a015/prioritize`);
    expect(request.request.method).toBe('POST');
    expect(request.request.body).toBeNull();
    request.flush({ error: false, message: 'Success' });
    expect(result).toBe('Success');
  });
});
