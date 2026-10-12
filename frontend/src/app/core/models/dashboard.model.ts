/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */


export type StarTarget =
  | { kind: 'project'; project: string }
  | { kind: 'task'; project: string; task: string }
  | { kind: 'cache'; cache: string };

