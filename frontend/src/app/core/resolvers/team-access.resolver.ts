/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, inject } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { ActivatedRoute, Data, ResolveFn } from '@angular/router';
import { map, tap } from 'rxjs';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TeamsService } from '@core/services/teams.service';
import { TeamResponse } from '@core/models';
import { denyOnRefusal } from '@core/routing/access-denied';
import { links } from '@shared/routing';

export interface TeamAccessData {
  team: TeamResponse;
}

export const teamAccessResolver: ResolveFn<TeamAccessData> = (route, state) => {
  const teams = inject(TeamsService);
  const crumbs = inject(BreadcrumbsService);
  const name = route.paramMap.get('team') ?? '';
  return teams.get(name).pipe(
    tap((team) => crumbs.rememberTeam(name, team.display_name)),
    map((team) => ({ team })),
    denyOnRefusal({ kind: 'team', name, back: String(links.teams()) }, state.url),
  );
};

const resolvedTeam = (data: Data): TeamResponse | undefined => (data['teamAccess'] as TeamAccessData | undefined)?.team;

export function injectTeam(): Signal<TeamResponse | undefined> {
  const route = inject(ActivatedRoute);
  return toSignal(route.data.pipe(map(resolvedTeam)), { initialValue: resolvedTeam(route.snapshot.data) });
}
