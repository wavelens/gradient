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
}

const CONNECT_REPOSITORY_WIZARD_UNFINISHED_INPUT_SESSION_STORAGE_KEY = 'gradient.connect-repository-draft';

function parse(raw: string | null): SavedConnectRepositoryWizardInput | null {
  if (!raw) return null;
  try {
    const value = JSON.parse(raw) as Partial<SavedConnectRepositoryWizardInput> | null;
    const form = value?.form;
    if (
      !value ||
      typeof value.project !== 'string' ||
      !WIZARD_STEPS.includes(value.step as ConnectRepositoryWizardStep) ||
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
    };
  } catch {
    return null;
  }
}

export function saveProjectWizardDraftToSessionStorage(input: SavedConnectRepositoryWizardInput): void {
  try {
    sessionStorage.setItem(CONNECT_REPOSITORY_WIZARD_UNFINISHED_INPUT_SESSION_STORAGE_KEY, JSON.stringify(input));
  } catch {
    return;
  }
}

export function readConnectRepositoryWizardInputFromSessionStorage(): SavedConnectRepositoryWizardInput | null {
  try {
    return parse(sessionStorage.getItem(CONNECT_REPOSITORY_WIZARD_UNFINISHED_INPUT_SESSION_STORAGE_KEY));
  } catch {
    return null;
  }
}

export function removeSavedConnectRepositoryWizardInput(): void {
  try {
    sessionStorage.removeItem(CONNECT_REPOSITORY_WIZARD_UNFINISHED_INPUT_SESSION_STORAGE_KEY);
  } catch {
    return;
  }
}

export function getShortRepositoryNameFromUrl(repository: string): string {
  return repositoryWebUrl(repository)?.replace(/^https?:\/\//, '') ?? repository.trim();
}
