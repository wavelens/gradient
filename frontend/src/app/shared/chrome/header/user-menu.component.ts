/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, output, viewChild } from '@angular/core';
import { MenuComponent, MenuItem } from '@gradient/ui/ui';
import { AuthService } from '@core/services/auth.service';
import { PendingUserDecisionsService } from '@core/services/pending-invites.service';
import { WhatsNewService } from '@core/whats-new/whats-new.service';
import { links } from '@shared/routing';

@Component({
  selector: 'app-user-menu',
  standalone: true,
  imports: [MenuComponent],
  template: `<gr-menu [model]="items()" />`,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class UserMenuComponent {
  private auth = inject(AuthService);
  private whatsNew = inject(WhatsNewService);
  private pending = inject(PendingUserDecisionsService);
  private menu = viewChild.required(MenuComponent);

  readonly isOpen = computed(() => this.menu().isOpen());
  readonly itemCommandSelected = output();

  protected items = computed<MenuItem[]>(() => ([
    { label: 'Settings', icon: 'settings', routerLink: links.settings('profile') },
    { label: this.pending.count() ? `Requests (${this.pending.count()})` : 'Requests', icon: 'mail', routerLink: links.settings('invites') },
    ...(this.whatsNew.hasNotes ? [{ label: "What's New", icon: 'new_releases', command: () => this.whatsNew.showAll() }] : []),
    ...(this.auth.user()?.superuser
      ? [
          { separator: true },
          { label: 'Event notifications', icon: 'webhook', routerLink: links.admin('webhooks') },
          { label: 'User access', icon: 'manage_accounts', routerLink: links.userAccessPageUrl() },
        ]
      : []),
    { separator: true },
    { label: 'Log out', icon: 'logout', command: () => this.auth.logout().subscribe() },
  ] as MenuItem[]).map((item) =>
    item.command ? { ...item, command: () => (this.itemCommandSelected.emit(), item.command?.()) } : item,
  ));

  toggle(origin: HTMLElement): void {
    if (this.menu().isOpen()) this.menu().hide();
    else this.menu().show(origin);
  }
}
