// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

import { bootstrapApplication } from '@angular/platform-browser';
import { appConfig } from './app/app.config';
import { App } from './app/app';
import { removeErrorPageRouterStateFromHistory } from './app/core/routing/access-denied';

removeErrorPageRouterStateFromHistory(window.history);
bootstrapApplication(App, appConfig)
  .catch((err) => console.error(err));
