/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable } from '@angular/core';
import { ActivatedRouteSnapshot, BaseRouteReuseStrategy, Params } from '@angular/router';

function sameParams(a: Params, b: Params): boolean {
  const keys = Object.keys(a);
  return keys.length === Object.keys(b).length && keys.every((k) => a[k] === b[k]);
}

// Detail pages read their path params once, so a jump to another entity of the same route must re-create them.
@Injectable()
export class ParamReuseStrategy extends BaseRouteReuseStrategy {
  override shouldReuseRoute(future: ActivatedRouteSnapshot, curr: ActivatedRouteSnapshot): boolean {
    return future.routeConfig === curr.routeConfig && sameParams(future.params, curr.params);
  }
}
