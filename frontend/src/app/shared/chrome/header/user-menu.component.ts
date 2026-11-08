/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, viewChild } from '@angular/core';
import { MenuComponent, MenuItem } from '@gradient/ui/ui';
import { AuthService } from '@core/services/auth.service';
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
  private menu = viewChild.required(MenuComponent);

  readonly isOpen = computed(() => this.menu().isOpen());

  protected items = computed<MenuItem[]>(() => [
    { label: 'Settings', icon: 'settings', routerLink: links.settings('profile') },
    { label: 'My Invites', icon: 'mail', routerLink: links.settings('invites') },
    ...(this.whatsNew.hasNotes ? [{ label: "What's New", icon: 'new_releases', command: () => this.whatsNew.showAll() }] : []),
    { separator: true },
    { label: 'Log out', icon: 'logout', command: () => this.auth.logout().subscribe() },
  ]);

  toggle(origin: HTMLElement): void {
    if (this.menu().isOpen()) this.menu().hide();
    else this.menu().show(origin);
  }
}
