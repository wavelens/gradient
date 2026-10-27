/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  TeamSummary,
  TeamResponse,
  PatchTeamRequest,
  TeamMemberItem,
  TeamRole,
  PendingInvitationItem,
  TeamWorkerEntry,
  RegisterWorkerRequest,
  PatchWorkerRequest,
  TeamGrants,
  TeamRequestItem,
  TeamGrantEntry,
  GrantProjectRequest,
  RegisteredTeamWorker,
  PatchProjectGrantRequest,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class TeamsService {
  private api = inject(ApiService);

  list(): Observable<TeamSummary[]> {
    return this.api.get<TeamSummary[]>('teams');
  }

  checkTeamNameAvailable(name: string): Observable<boolean> {
    return this.api.get<boolean>(`teams/available?name=${encodeURIComponent(name)}`);
  }

  create(name: string, displayName: string): Observable<string> {
    return this.api.put<string>('teams', { name, display_name: displayName });
  }

  get(team: string): Observable<TeamResponse> {
    return this.api.get<TeamResponse>(`teams/${team}`);
  }

  update(team: string, patch: PatchTeamRequest): Observable<string> {
    return this.api.patch<string>(`teams/${team}`, patch);
  }

  remove(team: string): Observable<string> {
    return this.api.delete<string>(`teams/${team}`);
  }

  members(team: string): Observable<TeamMemberItem[]> {
    return this.api.get<TeamMemberItem[]>(`teams/${team}/members`);
  }

  addMember(team: string, user: string, role: TeamRole): Observable<string> {
    return this.api.post<string>(`teams/${team}/members`, { user, role });
  }

  updateMember(team: string, user: string, role: TeamRole): Observable<string> {
    return this.api.patch<string>(`teams/${team}/members`, { user, role });
  }

  removeMember(team: string, user: string): Observable<string> {
    return this.api.delete<string>(`teams/${team}/members`, { user });
  }

  invitations(team: string): Observable<PendingInvitationItem[]> {
    return this.api.get<PendingInvitationItem[]>(`teams/${team}/invitations`);
  }

  revokeInvitation(team: string, user: string): Observable<string> {
    return this.api.delete<string>(`teams/${team}/invitations`, { user });
  }

  workers(team: string): Observable<TeamWorkerEntry[]> {
    return this.api.get<TeamWorkerEntry[]>(`teams/${team}/workers`);
  }

  registerWorker(team: string, worker: RegisterWorkerRequest): Observable<RegisteredTeamWorker> {
    return this.api.post<RegisteredTeamWorker>(`teams/${team}/workers`, worker);
  }

  updateWorker(team: string, workerId: string, patch: PatchWorkerRequest): Observable<string> {
    return this.api.patch<string>(`teams/${team}/workers/${workerId}`, patch);
  }

  removeWorker(team: string, workerId: string): Observable<string> {
    return this.api.delete<string>(`teams/${team}/workers/${workerId}`);
  }

  grants(team: string): Observable<TeamGrants> {
    return this.api.get<TeamGrants>(`teams/${team}/grants`);
  }

  requests(team: string): Observable<TeamRequestItem[]> {
    return this.api.get<TeamRequestItem[]>(`teams/${team}/requests`);
  }

  approveRequest(team: string, id: string): Observable<string> {
    return this.api.post<string>(`teams/${team}/requests/${id}`, {});
  }

  denyRequest(team: string, id: string): Observable<string> {
    return this.api.delete<string>(`teams/${team}/requests/${id}`);
  }

  projectGrants(project: string): Observable<TeamGrantEntry[]> {
    return this.api.get<TeamGrantEntry[]>(`projects/${project}/teams`);
  }

  grantProject(project: string, grant: GrantProjectRequest): Observable<string> {
    return this.api.post<string>(`projects/${project}/teams`, grant);
  }

  updateProjectGrant(project: string, team: string, patch: PatchProjectGrantRequest): Observable<string> {
    return this.api.patch<string>(`projects/${project}/teams/${team}`, patch);
  }

  removeProjectGrant(project: string, team: string): Observable<string> {
    return this.api.delete<string>(`projects/${project}/teams/${team}`);
  }

  cacheGrants(cache: string): Observable<TeamGrantEntry[]> {
    return this.api.get<TeamGrantEntry[]>(`caches/${cache}/teams`);
  }

  grantCache(cache: string, team: string, role: string): Observable<string> {
    return this.api.post<string>(`caches/${cache}/teams`, { team, role });
  }

  updateCacheGrant(cache: string, team: string, role: string): Observable<string> {
    return this.api.patch<string>(`caches/${cache}/teams/${team}`, { role });
  }

  removeCacheGrant(cache: string, team: string): Observable<string> {
    return this.api.delete<string>(`caches/${cache}/teams/${team}`);
  }
}
