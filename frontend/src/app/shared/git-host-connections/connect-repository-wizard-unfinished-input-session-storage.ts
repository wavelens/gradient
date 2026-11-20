/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { repositoryWebUrl } from '@shared/evaluation';

export type ConnectRepositoryWizardStep = 'repository' | 'git-host' | 'access' | 'triggers';

const WIZARD_STEPS: readonly ConnectRepositoryWizardStep[] = ['repository', 'git-host', 'access', 'triggers'];

export interface SavedConnectRepositoryWizardInput {
  project: string;
  step: ConnectRepositoryWizardStep;
  form: {
    repository: string;
    display_name: string;
    name: string;
    wildcard: string;
    branch: string;
    polling: boolean;
    cron: string;
    exec_enabled: boolean;
  };
  selectedConnectionId: string | null;
  savedAt: number;
}

const unfinishedConnectRepositoryInputSessionStorageKeyPrefix = 'gradient.connect-repository-draft:';

function storageKey(project: string): string {
  return unfinishedConnectRepositoryInputSessionStorageKeyPrefix + project;
}

function parse(raw: string | null): SavedConnectRepositoryWizardInput | null {
  if (!raw) return null;
  try {
    const value = JSON.parse(raw) as Partial<SavedConnectRepositoryWizardInput> | null;
    const form = value?.form;
    if (
      !value ||
      typeof value.project !== 'string' ||
      !WIZARD_STEPS.includes(value.step as ConnectRepositoryWizardStep) ||
      typeof value.savedAt !== 'number' ||
      !form ||
      typeof form.repository !== 'string' ||
      typeof form.display_name !== 'string' ||
      typeof form.name !== 'string' ||
      typeof form.wildcard !== 'string' ||
      typeof form.branch !== 'string' ||
      typeof form.polling !== 'boolean' ||
      typeof form.cron !== 'string' ||
      typeof form.exec_enabled !== 'boolean'
    ) {
      return null;
    }
    return {
      project: value.project,
      step: value.step as ConnectRepositoryWizardStep,
      form: { ...form },
      selectedConnectionId: typeof value.selectedConnectionId === 'string' ? value.selectedConnectionId : null,
      savedAt: value.savedAt,
    };
  } catch {
    return null;
  }
}

export function saveProjectWizardDraftToSessionStorage(input: SavedConnectRepositoryWizardInput): void {
  try {
    sessionStorage.setItem(storageKey(input.project), JSON.stringify(input));
  } catch {
    return;
  }
}

export function readProjectConnectRepositoryDraftFromSessionStorage(project: string): SavedConnectRepositoryWizardInput | null {
  try {
    const saved = parse(sessionStorage.getItem(storageKey(project)));
    return saved?.project === project ? saved : null;
  } catch {
    return null;
  }
}

export function removeProjectWizardDraftFromSessionStorage(project: string): void {
  try {
    sessionStorage.removeItem(storageKey(project));
  } catch {
    return;
  }
}

export function getMostRecentlySavedUnfinishedConnectRepositoryWizardInputAcrossProjects(): SavedConnectRepositoryWizardInput | null {
  try {
    let newest: SavedConnectRepositoryWizardInput | null = null;
    for (let index = 0; index < sessionStorage.length; index++) {
      const key = sessionStorage.key(index);
      if (!key?.startsWith(unfinishedConnectRepositoryInputSessionStorageKeyPrefix)) continue;
      const saved = parse(sessionStorage.getItem(key));
      if (saved && (!newest || saved.savedAt > newest.savedAt)) newest = saved;
    }
    return newest;
  } catch {
    return null;
  }
}

export function getShortRepositoryNameFromUrl(repository: string): string {
  return repositoryWebUrl(repository)?.replace(/^https?:\/\//, '') ?? repository.trim();
}
