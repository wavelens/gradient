/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { expect, test, type WebSocketRoute } from '@playwright/test';

test('the job board reconnects its live updates after the connection drops', async ({ page }) => {
  const liveConnections: WebSocketRoute[] = [];
  await page.routeWebSocket(/\/api\/v1\/board\/live$/, (route) => {
    liveConnections.push(route);
    route.connectToServer();
  });

  await page.goto('/board');
  const connected = page.getByRole('status', { name: 'Live updates connected' });
  const disconnected = page.getByRole('status', { name: /^Live updates disconnected/ });
  await expect(connected).toBeVisible();
  const connectionsBeforeDrop = liveConnections.length;

  await liveConnections[connectionsBeforeDrop - 1].close({ code: 1011, reason: 'dropped by the browser test' });
  await expect(disconnected).toBeVisible();
  await expect(connected).toBeVisible();
  expect(liveConnections.length).toBeGreaterThan(connectionsBeforeDrop);

  await expect(page.getByRole('heading', { level: 1, name: 'Job Board' })).toBeVisible();
  await expect(page.getByRole('main').getByRole('alert')).toHaveCount(0);
});
