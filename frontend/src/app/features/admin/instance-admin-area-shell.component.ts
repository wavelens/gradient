/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component } from '@angular/core';
import { RouterOutlet } from '@angular/router';
import { LinkTabsComponent, type LinkTab } from '@gradient/ui/ui';
import { links } from '@shared/routing';

@Component({
  selector: 'app-instance-admin-area-shell',
  standalone: true,
  imports: [RouterOutlet, LinkTabsComponent],
  template: `
    <div class="admin">
      <h1>Administration</h1>
      <gr-link-tabs class="admin-nav" [tabs]="tabs" ariaLabel="Page for the Admin Area" />
      <router-outlet></router-outlet>
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './admin-area.scss',
})
export class InstanceAdminAreaShellComponent {
  protected readonly tabs: LinkTab[] = [
    { label: 'Overview', link: links.admin('overview') },
    { label: 'Workers', link: links.admin('workers') },
    { label: 'Storage', link: links.admin('storage') },
    { label: 'Git hosts', link: links.admin('git-hosts') },
    { label: 'Settings', link: links.admin('settings') },
  ];
}
