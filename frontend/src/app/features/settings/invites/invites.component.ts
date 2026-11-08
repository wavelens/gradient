/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterLink, type UrlTree } from '@angular/router';
import { UserService } from '@core/services/user.service';
import { NotifyService } from '@core/services/notify.service';
import { PendingInvitesService } from '@core/services/pending-invites.service';
import { links } from '@shared/routing';
import { InviteItem } from '@core/models';
import { ConfirmService } from '@shared/confirm';
import {
  BadgeComponent,
  ButtonComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';
import { SettingsNavComponent } from '../settings-nav/settings-nav.component';

@Component({
  selector: 'app-invites',
  standalone: true,
  imports: [
    SettingsNavComponent,
    CommonModule,
    ServerTimestampToLocalDateTimePipe,
    RouterLink,
    BadgeComponent,
    ButtonComponent,
    LoadStateComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
  ],
  templateUrl: './invites.component.html',
  styleUrl: './invites.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class InvitesComponent implements OnInit {
  protected readonly links = links;
  private userService = inject(UserService);
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private pending = inject(PendingInvitesService);

  loading = signal(true);
  pendingToken = signal<string | null>(null);
  invites = signal<InviteItem[]>([]);
  joined = signal<InviteItem[]>([]);
  loadError = signal<unknown>(null);

  ngOnInit(): void {
    this.load();

    const token = this.route.snapshot.queryParamMap.get('token');
    if (token) {
      void this.router.navigateByUrl(links.settings('invites'), { replaceUrl: true });
      this.accept(token);
    }
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.userService.getInvites().subscribe({
      next: (invites) => {
        this.invites.set(invites);
        this.pending.show(invites);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  accept(token: string): void {
    this.pendingToken.set(token);
    this.userService.acceptInvite(token).subscribe({
      next: () => {
        this.pendingToken.set(null);
        this.notify.success('Invitation accepted');
        const invite = this.invites().find((item) => item.token === token);
        if (invite) this.joined.update((joined) => [invite, ...joined]);
        this.load();
      },
      error: (err: unknown) => {
        this.pendingToken.set(null);
        this.notify.failure('Invitation accept failed', err);
      },
    });
  }

  joinedLink(invite: InviteItem): UrlTree {
    switch (invite.kind) {
      case 'project': return links.project(invite.scope);
      case 'cache': return links.cache(invite.scope);
      case 'team': return links.team(invite.scope);
    }
  }

  decline(invite: InviteItem): void {
    this.confirm
      .confirm({
        title: 'Decline Invitation',
        target: invite.scope_display_name,
        consequence: 'Joining later needs a new invitation.',
        confirmLabel: 'Decline',
        failure: 'Failed to decline the invitation.',
        action: () => this.userService.declineInvite(invite.token),
      })
      .subscribe(() => {
        this.notify.success('Invitation declined');
        this.load();
      });
  }
}
