/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { describe, expect, it } from 'vitest';
import type { Worker } from './worker.model';
import { connectWaitState, gradientCiConnectUrl, gradientCiEntry, listedWorkers } from './gradient-ci.model';

function worker(overrides: Partial<Worker>): Worker {
  return {
    worker_id: 'w1',
    display_name: 'Builder',
    managed: false,
    active: true,
    gradient_ci: false,
    connected: false,
    enable_fetch: true,
    enable_eval: true,
    enable_build: true,
    ...overrides,
  };
}

describe('gradientCiEntry', () => {
  const registration = worker({ worker_id: 'g1', gradient_ci: true });
  const ofTeam = worker({ worker_id: 'g2', gradient_ci: true, team: 'platform' });

  it('offers Connect when nothing is connected', () => {
    expect(gradientCiEntry(true, [worker({})])).toEqual({ state: 'connect', worker: null });
  });

  it('shows a project connection and a team connection as connected', () => {
    expect(gradientCiEntry(true, [registration]).state).toBe('connected');
    expect(gradientCiEntry(true, [ofTeam])).toEqual({ state: 'connected', worker: ofTeam });
  });

  it('hides the offer when the option is off', () => {
    expect(gradientCiEntry(false, []).state).toBe('hidden');
  });

  it('keeps a connection listed when the option is off', () => {
    expect(gradientCiEntry(false, [registration])).toEqual({ state: 'connected', worker: registration });
  });
});

describe('listedWorkers', () => {
  it('lists every worker but the one shown in the Gradient.CI entry', () => {
    const registration = worker({ worker_id: 'g1', gradient_ci: true });
    const own = worker({ worker_id: 'w1' });
    const workers = [registration, own];
    expect(listedWorkers(workers, gradientCiEntry(true, workers))).toEqual([own]);
  });
});

describe('gradientCiConnectUrl', () => {
  it('opens /connect with the scope and the label', () => {
    expect(gradientCiConnectUrl('https://servers.gradient.ci', 'project', 'ci.example.com / acme')).toBe(
      'https://servers.gradient.ci/connect?scope=project&label=ci.example.com+%2F+acme',
    );
  });
});

describe('connectWaitState', () => {
  it('is online as soon as the worker is connected', () => {
    expect(connectWaitState({ connected: true }, 1_000)).toBe('online');
  });

  it('waits up to 30 s, then reports offline', () => {
    expect(connectWaitState(undefined, 29_999)).toBe('waiting');
    expect(connectWaitState({ connected: false }, 30_000)).toBe('offline');
  });
});
