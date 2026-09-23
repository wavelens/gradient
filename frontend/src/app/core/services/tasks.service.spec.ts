/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { TasksService, ReportOptions } from './tasks.service';
import { environment } from '@environments/environment';

const evaluation = '01a05a38-3276-7252-bc05-c139d9c8a015';

describe('TasksService.downloadReport', () => {
  let service: TasksService;
  let httpMock: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [TasksService, provideHttpClient(), provideHttpClientTesting()],
    });
    service = TestBed.inject(TasksService);
    httpMock = TestBed.inject(HttpTestingController);
  });

  afterEach(() => httpMock.verify());

  function requestFor(options: ReportOptions): URLSearchParams {
    service.downloadReport(evaluation, options).subscribe();
    const request = httpMock.expectOne((r) =>
      r.url.startsWith(`${environment.apiUrl}/evals/${evaluation}/report`)
    );
    request.flush(new Blob(['report']));
    return new URLSearchParams(request.request.url.split('?')[1] ?? '');
  }

  // The dialog asks what to put in; the wire asks what to leave out. Every
  // box ticked has to mean the fullest report the caller may have.
  it('sends the wire its anonymise flags inverted', () => {
    const params = requestFor({
      include_identities: true,
      include_packages: true,
      include_logs: true,
      include_instance: true,
    });
    expect(params.get('anonymize_identities')).toBe('false');
    expect(params.get('anonymize_packages')).toBe('false');
    expect(params.get('include_logs')).toBe('true');
    expect(params.get('include_instance')).toBe('true');
  });

  it('anonymises what the caller left unticked', () => {
    const params = requestFor({
      include_identities: false,
      include_packages: false,
      include_logs: false,
      include_instance: false,
    });
    expect(params.get('anonymize_identities')).toBe('true');
    expect(params.get('anonymize_packages')).toBe('true');
    expect(params.get('include_logs')).toBe('false');
    expect(params.get('include_instance')).toBe('false');
  });
});

describe('TasksService.startEvaluation', () => {
  let service: TasksService;
  let httpMock: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [TasksService, provideHttpClient(), provideHttpClientTesting()],
    });
    service = TestBed.inject(TasksService);
    httpMock = TestBed.inject(HttpTestingController);
  });

  afterEach(() => httpMock.verify());

  function bodyFor(start: () => void): unknown {
    start();
    const request = httpMock.expectOne(`${environment.apiUrl}/tasks/p/t/evaluate`);
    request.flush({ error: false, message: evaluation });
    return request.request.body;
  }

  it('leaves the walk to the server default', () => {
    expect(bodyFor(() => service.startEvaluation('p', 't').subscribe())).toBeFalsy();
  });

  it('asks for a full rewalk', () => {
    expect(bodyFor(() => service.startEvaluation('p', 't', 'full').subscribe()))
      .toEqual({ walk: 'full' });
  });
});
