/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { version } from '../../package.json';

export const environment = {
  production: true,
  apiUrl: '/api/v1',
  version,
  oidcEnabled: false,
  registrationDisabled: false,
};
