/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { environment } from '@environments/environment';
import type { GitHostConnectionOwner } from '@core/models';
import { GitHostConnectionsApi } from './git-host-connections.service';

const apiUrl = environment.apiUrl;
const teamGitHostConnectionOwner: GitHostConnectionOwner = { type: 'team', name: 'platform' };

describe('GitHostConnectionsApi repository grants', () => {
  let gitHostConnectionsApi: GitHostConnectionsApi;
  let httpMock: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [GitHostConnectionsApi, provideHttpClient(), provideHttpClientTesting()],
    });
    gitHostConnectionsApi = TestBed.inject(GitHostConnectionsApi);
    httpMock = TestBed.inject(HttpTestingController);
  });

  afterEach(() => httpMock.verify());

  it('lists the grants of an instance connection under the admin path', () => {
    gitHostConnectionsApi.getGitHostConnectionRepositoryGrants({ type: 'instance' }, 'connection-1').subscribe();
    const request = httpMock.expectOne(`${apiUrl}/admin/git-host-connections/connection-1/repository-grants`);
    expect(request.request.method).toBe('GET');
    request.flush({ error: false, message: [] });
  });

  it('creates a grant of a team connection with PUT and the body', () => {
    const body = { project: 'web', repository_owner: 'acme', repository_name: 'shop' };
    gitHostConnectionsApi.addRepositoryGrantToGitHostConnection(teamGitHostConnectionOwner, 'connection-1', body).subscribe();
    const request = httpMock.expectOne(`${apiUrl}/teams/platform/git-host-connections/connection-1/repository-grants`);
    expect(request.request.method).toBe('PUT');
    expect(request.request.body).toEqual(body);
    request.flush({ error: false, message: {} });
  });

  it('deletes a grant by its id', () => {
    gitHostConnectionsApi.deleteRepositoryGrantOfGitHostConnection(teamGitHostConnectionOwner, 'connection-1', 'grant-1').subscribe();
    const request = httpMock.expectOne(`${apiUrl}/teams/platform/git-host-connections/connection-1/repository-grants/grant-1`);
    expect(request.request.method).toBe('DELETE');
    request.flush({ error: false, message: true });
  });
});
