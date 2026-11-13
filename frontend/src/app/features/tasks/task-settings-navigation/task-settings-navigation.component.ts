/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { SettingsPageSideNavigationComponent, type VerticalNavigationLink } from '@gradient/ui/ui';
import { injectTaskAccessData } from '@core/resolvers/inject-access';
import { canOpenProjectSettings, isViewerProjectMember } from '@shared/access';
import { links } from '@shared/routing';

@Component({
  selector: 'app-task-settings-navigation',
  standalone: true,
  imports: [SettingsPageSideNavigationComponent],
  template: `<gr-settings-page-side-navigation ariaLabel="Task Settings" [links]="navigationLinkItems()" />`,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class TaskSettingsNavigationComponent {
  projectUrlName = input.required<string>();
  taskUrlName = input.required<string>();

  private resolvedTask = injectTaskAccessData();

  protected navigationLinkItems = computed<VerticalNavigationLink[]>(() => {
    const project = this.projectUrlName();
    const task = this.taskUrlName();
    const resolvedTask = this.resolvedTask()?.task;
    const general = { label: 'General', icon: 'tune', link: links.taskPage(project, task, 'settings') };
    if (!isViewerProjectMember(resolvedTask)) return [general];
    return [
      general,
      { label: 'Repository', icon: 'hub', link: links.taskPage(project, task, 'repository') },
      { label: 'Triggers', icon: 'schedule', link: links.taskPage(project, task, 'triggers') },
      { label: 'Actions', icon: 'bolt', link: links.taskPage(project, task, 'actions') },
      { label: 'Flake inputs', icon: 'share', link: links.taskPage(project, task, 'flake-inputs') },
      ...(canOpenProjectSettings(resolvedTask)
        ? [{ label: 'Project Settings', icon: 'apartment', link: links.projectPage(project, 'settings'), showSeparatorLineBeforeLink: true }]
        : []),
    ];
  });
}
