/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { WaitingReason } from '@core/models';
import { type WaitingAccess, explainWaiting } from './waiting-reason';

const ADMIN: WaitingAccess = { manageWorkers: true, manageSubscriptions: true, trigger: true, superuser: false };
const VIEWER: WaitingAccess = { manageWorkers: false, manageSubscriptions: false, trigger: false, superuser: false };
const SUPERUSER: WaitingAccess = { manageWorkers: true, manageSubscriptions: true, trigger: true, superuser: true };

function linkOf(reason: WaitingReason | null, access: WaitingAccess): string | null {
  const action = explainWaiting(reason, 'acme', access).action;
  return action?.kind === 'link' ? String(action.link) : null;
}

describe('explainWaiting', () => {
  it('sends a project admin to the cache subscriptions when no cache is configured', () => {
    const explained = explainWaiting({ kind: 'no_cache' }, 'acme', ADMIN);
    expect(explained.title).toBe('Cache not configured');
    expect(explained.action).toEqual(expect.objectContaining({ kind: 'link', label: 'Configure cache' }));
    expect(linkOf({ kind: 'no_cache' }, ADMIN)).toBe('/project/acme/caches');
    expect(explained.hint).toBeNull();
  });

  it('tells a viewer whom to ask instead of offering a page they cannot use', () => {
    const explained = explainWaiting({ kind: 'no_cache' }, 'acme', VIEWER);
    expect(explained.action).toBeNull();
    expect(explained.hint).toBe('Ask a project admin to subscribe this project to a cache.');
  });

  it('sends a project admin to register a worker for every missing worker kind', () => {
    const reasons: WaitingReason[] = [
      { kind: 'workers', unmet: [], connected_workers: 0, available_architectures: [] },
      { kind: 'eval_workers', capability: 'network', connected_workers: 0 },
      { kind: 'exec_workers', unmet: { labels: ['gpu'], devices: [], mounts: [], nodes: 0, custom_image: false }, connected_workers: 1, matching_workers: 0 },
    ];
    for (const reason of reasons) expect(linkOf(reason, ADMIN)).toBe('/project/acme/workers');
    expect(linkOf(null, ADMIN)).toBe('/project/acme/workers');
  });

  it('scopes the connected worker count to the project', () => {
    const explained = explainWaiting({ kind: 'eval_workers', capability: 'eval', connected_workers: 0 }, 'acme', ADMIN);
    expect(explained.detail).toBe('This project has no workers connected to run the evaluation.');
  });

  it('lists the unmet requirements without a dangling colon in the detail', () => {
    const explained = explainWaiting({
      kind: 'workers',
      unmet: [{ architecture: 'aarch64-linux', required_features: ['kvm'], build_count: 3 }],
      connected_workers: 2,
      available_architectures: ['x86_64-linux'],
    }, 'acme', ADMIN);
    expect(explained.detail).toBe("2 connected workers (x86_64-linux) can't build this evaluation.");
    expect(explained.requirements).toEqual(['aarch64-linux with feature kvm - 3 builds']);
  });

  it('names the worker an evaluation waits for and what it must do', () => {
    const explained = explainWaiting({ kind: 'eval_workers', capability: 'secrets', connected_workers: 1 }, 'acme', ADMIN);
    expect(explained.title).toBe('Waiting for worker with secrets');
    expect(explained.detail).toBe('1 worker is connected, but none can fetch the flake sources with the project SSH key.');
  });

  it('gives exec jobs their own titles and offers Run for a manual start to those who may start runs', () => {
    expect(explainWaiting({ kind: 'manual_start' }, 'acme', ADMIN).action).toEqual(expect.objectContaining({ kind: 'start', label: 'Run' }));
    expect(explainWaiting({ kind: 'manual_start' }, 'acme', VIEWER).hint).toContain('permission to start runs');
    const exec = explainWaiting({
      kind: 'exec_workers',
      unmet: { labels: ['3d-printer'], devices: [{ name: 'gpu:h100', count: 16 }], mounts: [], nodes: 0, custom_image: false },
      connected_workers: 1,
      matching_workers: 0,
    }, 'acme', ADMIN);
    expect(exec.title).toBe('Waiting for exec worker');
    expect(exec.requirements).toEqual(['label 3d-printer', 'gpu:h100:16']);
  });

  it('counts the exec workers whose devices are busy', () => {
    const explained = explainWaiting({
      kind: 'exec_workers',
      unmet: { labels: [], devices: [], mounts: [], nodes: 0, custom_image: false },
      connected_workers: 3,
      matching_workers: 1,
    }, 'acme', ADMIN);
    expect(explained.detail).toBe('Waiting for free devices on 1 worker.');
  });

  it('sends only an instance admin to stop draining', () => {
    expect(linkOf({ kind: 'draining' }, SUPERUSER)).toBe('/board/health');
    expect(explainWaiting({ kind: 'draining' }, 'acme', ADMIN).hint).toBe('Ask an instance admin to stop draining.');
  });

  it('explains how a maintainer approves a pull request, which happens outside Gradient', () => {
    const explained = explainWaiting({ kind: 'approval', pr_number: 7, pr_author: 'mallory' }, 'acme', SUPERUSER);
    expect(explained.detail).toBe('Awaiting maintainer approval for pull request #7 by mallory.');
    expect(explained.action).toBeNull();
    expect(explained.hint).toContain('/gradient approve');
  });

  it('offers nothing to do while the build graph recovers by itself', () => {
    const explained = explainWaiting({ kind: 'graph_stuck', pending_shared_builds: 9 }, 'acme', SUPERUSER);
    expect(explained.detail).toBe('Workers are available, but 9 builds are blocked on dependencies. Recovering automatically.');
    expect(explained.action).toBeNull();
    expect(explained.hint).toBeNull();
  });

  it('names the upstream caches that do not answer', () => {
    const explained = explainWaiting({
      kind: 'upstream_caches',
      pending_shared_builds: 14,
      failing_upstream_caches: ['https://attic.example/lantian'],
    }, 'acme', VIEWER);
    expect(explained.detail).toBe('14 builds wait for an answer from the upstream caches. Not answering: https://attic.example/lantian.');
    expect(explained.hint).toContain('cache admin');
  });
});
