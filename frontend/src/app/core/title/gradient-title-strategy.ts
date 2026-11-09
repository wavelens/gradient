/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, Injector, effect, inject, signal } from '@angular/core';
import { Title } from '@angular/platform-browser';
import { ActivatedRouteSnapshot, RouterStateSnapshot, TitleStrategy } from '@angular/router';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { TaskAccessData } from '@core/resolvers/task-access.resolver';
import { CacheAccessData } from '@core/resolvers/cache-access.resolver';
import { ProjectAccessData } from '@core/resolvers/project-access.resolver';
import { TeamAccessData } from '@core/resolvers/team-access.resolver';

const BRAND = 'Gradient';
const ENTITY_ONLY_ROUTE_TITLES = new Set(['Project', 'Task', 'Cache', 'Team']);

export interface BrowserTabTitleParts {
  kind: 'project' | 'task' | 'cache' | 'team';
  urlNames: string[];
  displayName: string;
}

@Injectable({ providedIn: 'root' })
export class GradientTitleStrategy extends TitleStrategy {
  private readonly title = inject(Title);
  private readonly injector = inject(Injector);
  private readonly page = signal<{ entity: BrowserTabTitleParts | undefined; routeTitle: string | undefined } | null>(null);

  constructor() {
    super();
    effect(() => {
      const page = this.page();
      if (!page) return;
      const { entity, routeTitle } = page;
      const renamed = entity && this.injector.get(BreadcrumbsService).getLastKnownDisplayNameSignal(entity.kind, ...entity.urlNames)?.();
      this.title.setTitle(composeTitle(renamed || entity?.displayName, routeTitle));
    });
  }

  override updateTitle(state: RouterStateSnapshot): void {
    this.page.set({ entity: findFirstResolvedEntityInRouterSnapshotTree(state.root), routeTitle: this.buildTitle(state) });
  }
}

export function composeTitle(entity: string | undefined, routeTitle: string | undefined): string {
  if (entity && routeTitle && !ENTITY_ONLY_ROUTE_TITLES.has(routeTitle)) {
    return `${entity} · ${routeTitle} · ${BRAND}`;
  }
  if (entity) return `${entity} · ${BRAND}`;
  if (routeTitle) return `${routeTitle} · ${BRAND}`;
  return BRAND;
}

export function findFirstResolvedEntityInRouterSnapshotTree(snapshot: ActivatedRouteSnapshot): BrowserTabTitleParts | undefined {
  const visit = (s: ActivatedRouteSnapshot): BrowserTabTitleParts | undefined => {
    const data = s.data as {
      taskAccess?: TaskAccessData;
      cacheAccess?: CacheAccessData;
      projectAccess?: ProjectAccessData;
      teamAccess?: TeamAccessData;
    };
    const param = (name: string) => s.paramMap.get(name) ?? '';
    if (data.taskAccess) {
      return { kind: 'task', urlNames: [param('project'), param('task')], displayName: data.taskAccess.task.display_name };
    }
    if (data.cacheAccess) return { kind: 'cache', urlNames: [param('cache')], displayName: data.cacheAccess.cache.display_name };
    if (data.projectAccess?.project) {
      return { kind: 'project', urlNames: [param('project')], displayName: data.projectAccess.project.display_name };
    }
    if (data.teamAccess) return { kind: 'team', urlNames: [param('team')], displayName: data.teamAccess.team.display_name };
    for (const child of s.children) {
      const found = visit(child);
      if (found) return found;
    }
    return undefined;
  };
  return visit(snapshot);
}
