/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { HttpTestingController } from '@angular/common/http/testing';
import { AdminService, AdminTask, StartDeepGcResponse } from './admin.service';
import { environment } from '@environments/environment';

const apiUrl = environment.apiUrl;

const sampleTask: AdminTask = {
  id: 'task-1',
  kind: 'deep-gc',
  status: 'pending',
  created_at: '2026-01-01T00:00:00Z',
  started_at: null,
  finished_at: null,
  progress: null,
  error: null,
  created_by: 'user-1',
};

describe('AdminService', () => {
  let service: AdminService;
  let httpMock: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [AdminService, provideHttpClient(), provideHttpClientTesting()],
    });
    service = TestBed.inject(AdminService);
    httpMock = TestBed.inject(HttpTestingController);
  });

  afterEach(() => httpMock.verify());

  it('startDeepGc() POSTs to admin/maintenance/deep-gc and unwraps the response', () => {
    let result: StartDeepGcResponse | undefined;
    service.startDeepGc().subscribe((v) => (result = v));

    const req = httpMock.expectOne(`${apiUrl}/admin/maintenance/deep-gc`);
    expect(req.request.method).toBe('POST');
    req.flush({ error: false, message: { task_id: 't1', status: 'pending' } });

    expect(result).toEqual({ task_id: 't1', status: 'pending' });
  });

  it('listTasks() GETs admin/tasks and returns the task array', () => {
    let result: AdminTask[] | undefined;
    service.listTasks().subscribe((v) => (result = v));

    const req = httpMock.expectOne(`${apiUrl}/admin/tasks`);
    expect(req.request.method).toBe('GET');
    req.flush({ error: false, message: [sampleTask] });

    expect(result?.length).toBe(1);
  });

  it('githubAppConfigured() emits true when credentials endpoint succeeds', () => {
    let result: boolean | undefined;
    service.githubAppConfigured().subscribe((v) => (result = v));

    const req = httpMock.expectOne(`${apiUrl}/admin/github-app/credentials`);
    req.flush({ error: false, message: { id: 1, slug: 'app', html_url: '', pem: '', webhook_secret: '', client_id: '', client_secret: '' } });

    expect(result).toBe(true);
  });

  it('githubAppConfigured() emits false when credentials endpoint returns 404', () => {
    let result: boolean | undefined;
    service.githubAppConfigured().subscribe((v) => (result = v));

    const req = httpMock.expectOne(`${apiUrl}/admin/github-app/credentials`);
    req.flush('nope', { status: 404, statusText: 'Not Found' });

    expect(result).toBe(false);
  });
});
