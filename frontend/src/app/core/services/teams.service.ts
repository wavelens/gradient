/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  GrantRequest,
  PatchTeam,
  PendingInvitation,
  RegisterTeamWorker,
  Team,
  TeamGrant,
  TeamGrants,
  TeamEvaluation,
  TeamMember,
  TeamRequest,
  TeamRole,
  TeamSummary,
  TeamWorker,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class TeamsService {
  private api = inject(ApiService);

  list(): Observable<TeamSummary[]> {
    return this.api.get<TeamSummary[]>('teams');
  }

  create(name: string, displayName: string): Observable<string> {
    return this.api.put<string>('teams', { name, display_name: displayName });
  }

  get(team: string): Observable<Team> {
    return this.api.get<Team>(`teams/${team}`);
  }

  evaluations(team: string): Observable<TeamEvaluation[]> {
    return this.api.get<TeamEvaluation[]>(`teams/${team}/evaluations`);
  }

  update(team: string, patch: PatchTeam): Observable<string> {
    return this.api.patch<string>(`teams/${team}`, patch);
  }

  remove(team: string): Observable<string> {
    return this.api.delete<string>(`teams/${team}`);
  }

  members(team: string): Observable<TeamMember[]> {
    return this.api.get<TeamMember[]>(`teams/${team}/members`);
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

  invitations(team: string): Observable<PendingInvitation[]> {
    return this.api.get<PendingInvitation[]>(`teams/${team}/invitations`);
  }

  revokeInvitation(team: string, user: string): Observable<string> {
    return this.api.delete<string>(`teams/${team}/invitations`, { user });
  }

  workers(team: string): Observable<TeamWorker[]> {
    return this.api.get<TeamWorker[]>(`teams/${team}/workers`);
  }

  registerWorker(team: string, worker: RegisterTeamWorker): Observable<{ team: string; token?: string }> {
    return this.api.post<{ team: string; token?: string }>(`teams/${team}/workers`, worker);
  }

  updateWorker(team: string, workerId: string, patch: { active?: boolean; display_name?: string }): Observable<string> {
    return this.api.patch<string>(`teams/${team}/workers/${workerId}`, patch);
  }

  removeWorker(team: string, workerId: string): Observable<string> {
    return this.api.delete<string>(`teams/${team}/workers/${workerId}`);
  }

  grants(team: string): Observable<TeamGrants> {
    return this.api.get<TeamGrants>(`teams/${team}/grants`);
  }

  requests(team: string): Observable<TeamRequest[]> {
    return this.api.get<TeamRequest[]>(`teams/${team}/requests`);
  }

  approveRequest(team: string, id: string): Observable<string> {
    return this.api.post<string>(`teams/${team}/requests/${id}`, {});
  }

  denyRequest(team: string, id: string): Observable<string> {
    return this.api.delete<string>(`teams/${team}/requests/${id}`);
  }

  projectGrants(project: string): Observable<TeamGrant[]> {
    return this.api.get<TeamGrant[]>(`projects/${project}/teams`);
  }

  grantProject(project: string, grant: GrantRequest): Observable<string> {
    return this.api.post<string>(`projects/${project}/teams`, grant);
  }

  updateProjectGrant(project: string, team: string, patch: Partial<Omit<GrantRequest, 'team'>>): Observable<string> {
    return this.api.patch<string>(`projects/${project}/teams/${team}`, patch);
  }

  removeProjectGrant(project: string, team: string): Observable<string> {
    return this.api.delete<string>(`projects/${project}/teams/${team}`);
  }

  cacheGrants(cache: string): Observable<TeamGrant[]> {
    return this.api.get<TeamGrant[]>(`caches/${cache}/teams`);
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
