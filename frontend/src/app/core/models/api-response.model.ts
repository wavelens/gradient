/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface ApiResponse<T> {
  error: boolean;
  message: T | string;
}

export interface Paginated<T> {
  items: T;
  total: number;
  page: number;
  per_page: number;
}
