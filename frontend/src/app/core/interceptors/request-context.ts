/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { HttpContext, HttpContextToken } from '@angular/common/http';

export const SESSION_PROBE = new HttpContextToken(() => false);
export const BACKGROUND = new HttpContextToken(() => false);

export function inBackground(): { context: HttpContext } {
  return { context: new HttpContext().set(BACKGROUND, true) };
}

export function asSessionProbe(): { context: HttpContext } {
  return { context: new HttpContext().set(SESSION_PROBE, true) };
}
