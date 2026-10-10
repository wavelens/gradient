/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { IsActiveMatchOptions } from '@angular/router';

function ignoringQuery(paths: IsActiveMatchOptions['paths']): IsActiveMatchOptions {
  return { paths, queryParams: 'ignored', matrixParams: 'ignored', fragment: 'ignored' };
}

const EXACT_PATH = ignoringQuery('exact');
const PATH_PREFIX = ignoringQuery('subset');

export function activeLinkMatch(exactPath: boolean): IsActiveMatchOptions {
  return exactPath ? EXACT_PATH : PATH_PREFIX;
}
