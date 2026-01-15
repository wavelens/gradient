/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface Commit {
  id: string;
  message: string;
  hash: string;
  author?: string;
  author_name: string;
}
