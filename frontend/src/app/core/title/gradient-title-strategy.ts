/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Title } from '@angular/platform-browser';
import { ActivatedRouteSnapshot, RouterStateSnapshot, TitleStrategy } from '@angular/router';
import { TaskAccessData } from '@core/resolvers/task-access.resolver';
import { CacheAccessData } from '@core/resolvers/cache-access.resolver';
import { ProjectAccessData } from '@core/resolvers/project-access.resolver';

const BRAND = 'Gradient';
const ENTITY_ONLY_ROUTE_TITLES = new Set(['Project', 'Task', 'Cache']);

@Injectable({ providedIn: 'root' })
export class GradientTitleStrategy extends TitleStrategy {
  private readonly title = inject(Title);

  override updateTitle(state: RouterStateSnapshot): void {
    const routeTitle = this.buildTitle(state);
    const entity = findEntityName(state.root);
    this.title.setTitle(composeTitle(entity, routeTitle));
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

export function findEntityName(snapshot: ActivatedRouteSnapshot): string | undefined {
  const visit = (s: ActivatedRouteSnapshot): string | undefined => {
    const data = s.data as {
      taskAccess?: TaskAccessData;
      cacheAccess?: CacheAccessData;
      projectAccess?: ProjectAccessData;
    };
    const name =
      data.taskAccess?.task.display_name ??
      data.cacheAccess?.cache.display_name ??
      data.projectAccess?.project?.display_name;
    if (name) return name;
    for (const child of s.children) {
      const found = visit(child);
      if (found) return found;
    }
    return undefined;
  };
  return visit(snapshot);
}
