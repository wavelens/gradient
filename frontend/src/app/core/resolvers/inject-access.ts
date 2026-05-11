/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, computed, inject } from '@angular/core';
import { ActivatedRoute, Data } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { Observable, map, of } from 'rxjs';
import { ProjectAccessData } from './project-access.resolver';
import { CacheAccessData } from './cache-access.resolver';
import { AccessState } from '@core/models/access.model';

const DEFAULT_ACCESS: AccessState = { managed: false, canEdit: false, canTrigger: false };

function parentDataSignal<T>(
  route: ActivatedRoute,
  key: 'projectAccess' | 'cacheAccess',
): Signal<T | undefined> {
  const source: Observable<T | undefined> = route.parent
    ? route.parent.data.pipe(map((d: Data) => d[key] as T | undefined))
    : of(undefined);
  return toSignal(source, { initialValue: undefined });
}

export function injectProjectAccessData(): Signal<ProjectAccessData | undefined> {
  const route = inject(ActivatedRoute);
  return parentDataSignal<ProjectAccessData>(route, 'projectAccess');
}

export function injectCacheAccessData(): Signal<CacheAccessData | undefined> {
  const route = inject(ActivatedRoute);
  return parentDataSignal<CacheAccessData>(route, 'cacheAccess');
}

export function injectProjectAccess(): Signal<AccessState> {
  const data = injectProjectAccessData();
  return computed(() => data()?.access ?? DEFAULT_ACCESS);
}

export function injectCacheAccess(): Signal<AccessState> {
  const data = injectCacheAccessData();
  return computed(() => data()?.access ?? DEFAULT_ACCESS);
}
