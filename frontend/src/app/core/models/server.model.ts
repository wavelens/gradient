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
  description: string;
  host: string;
  port: number;
  user: string;
  architecture: Architecture;
  features: string[];
  created_by?: string;
  created_at?: string;
}

export interface ServerConnectionTest {
  success: boolean;
  message: string;
}
