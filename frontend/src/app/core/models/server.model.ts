/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Architecture } from './build.model';

export interface Server {
  id: string;
  organization: string;
  name: string;
  active: boolean;
  display_name: string;
  host: string;
  port: number;
  username: string;
  last_connection_at?: string;
  managed: boolean;
  created_by?: string;
  created_at?: string;
}

export interface ServerConnectionTest {
  success: boolean;
  message: string;
}
