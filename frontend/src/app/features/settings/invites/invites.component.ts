/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { EntityNameComponent, entityKindToIconName, formatEntityLabel } from '@shared/ui';
import { ServerTimestampToLocalDateTimePipe, subscriptionModeLabel } from '@shared/text';
import { ActivatedRoute, Router, RouterLink, type UrlTree } from '@angular/router';
import { EMPTY, Observable, catchError, finalize, tap } from 'rxjs';
import { UserService } from '@core/services/user.service';
import { CachesService } from '@core/services/caches.service';
import { TeamsService } from '@core/services/teams.service';
import { NotifyService } from '@core/services/notify.service';
import { PendingUserDecisionsService } from '@core/services/pending-invites.service';
import { links } from '@shared/routing';
import type { InviteItem, SubscriptionRequestItem, TeamGrantRequestInboxEntry } from '@core/models';
import { ConfirmService } from '@shared/confirm';
import { teamRoleValueToDisplayLabel } from '@shared/access';
import {
  BadgeComponent,
  ButtonComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { SettingsNavComponent } from '../settings-nav/settings-nav.component';

@Component({
  selector: 'app-invites',
  standalone: true,
  imports: [
    SettingsNavComponent,
    EntityNameComponent,
    ServerTimestampToLocalDateTimePipe,
    RouterLink,
    BadgeComponent,
    ButtonComponent,
    LoadStateComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
    SettingsSectionComponent,
  ],
  templateUrl: './invites.component.html',
  styleUrl: './invites.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class RequestsPageComponent implements OnInit {
  protected readonly entityKindToIconName = entityKindToIconName;
  protected readonly links = links;
  protected readonly modeLabel = subscriptionModeLabel;
  private userService = inject(UserService);
  private caches = inject(CachesService);
  private teams = inject(TeamsService);
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  protected readonly pending = inject(PendingUserDecisionsService);

  loading = signal(true);
  pendingToken = signal<string | null>(null);
  requestKeyWithDecisionInProgress = signal<string | null>(null);
  joined = signal<InviteItem[]>([]);
  loadError = signal<unknown>(null);

  invites = this.pending.invites;
  cachedSubscriptionRequestsOnPage = computed(() => this.pending.all().subscription_requests);
  teamGrantRequestsOnPage = computed(() => this.pending.all().team_requests);

  ngOnInit(): void {
    const token = this.route.snapshot.queryParamMap.get('token');
    if (!token) {
      this.load();
      return;
    }
    void this.router.navigateByUrl(links.settings('invites'), { replaceUrl: true });
    this.loadPendingRequestsWithoutError().pipe(finalize(() => this.accept(token))).subscribe();
  }

  load(): void {
    this.loadPendingRequestsWithoutError().subscribe();
  }

  private loadPendingRequestsWithoutError(): Observable<unknown> {
    this.loading.set(true);
    this.loadError.set(null);
    return this.pending.fetchAndStorePendingDecisions().pipe(
      tap({
        next: () => this.loading.set(false),
        error: (error: unknown) => {
          this.loadError.set(error);
          this.loading.set(false);
        },
      }),
      catchError(() => EMPTY),
    );
  }

  accept(token: string): void {
    this.pendingToken.set(token);
    this.userService.acceptInvite(token).subscribe({
      next: () => {
        this.pendingToken.set(null);
        this.notify.success('Invitation accepted');
        const invite = this.invites().find((item) => item.token === token);
        if (invite) this.joined.update((joined) => [invite, ...joined]);
        this.removeDecidedInviteFromStoredPendingDecisions(token);
      },
      error: (err: unknown) => {
        this.pendingToken.set(null);
        this.notify.failure("Couldn't accept invitation", err);
      },
    });
  }

  getInviteRoleDisplayText(invite: InviteItem): string {
    return invite.kind === 'team' ? teamRoleValueToDisplayLabel(invite.role) : invite.role;
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
        failure: "Couldn't decline the invitation",
        action: () => this.userService.declineInvite(invite.token),
      })
      .subscribe(() => {
        this.notify.success('Invitation declined');
        this.removeDecidedInviteFromStoredPendingDecisions(invite.token);
      });
  }

  getCacheSubscriptionRequestRowKey(request: SubscriptionRequestItem): string {
    return `${request.cache}/${request.project}`;
  }

  approveSubscriptionRequestAndRemoveRow(request: SubscriptionRequestItem): void {
    this.requestKeyWithDecisionInProgress.set(this.getCacheSubscriptionRequestRowKey(request));
    this.caches.approveSubscriptionRequest(request.cache, request.project).subscribe({
      next: () => {
        this.requestKeyWithDecisionInProgress.set(null);
        this.notify.success('Subscription request approved');
        this.removeDecidedSubscriptionRequestRowFromStoredPendingDecisions(request);
      },
      error: (err: unknown) => {
        this.requestKeyWithDecisionInProgress.set(null);
        this.notify.failure("Couldn't approve subscription request", err);
      },
    });
  }

  confirmAndDenyCacheSubscriptionRequest(request: SubscriptionRequestItem): void {
    const project = formatEntityLabel({ display_name: request.project_display_name, name: request.project });
    const cache = formatEntityLabel({ display_name: request.cache_display_name, name: request.cache });
    this.confirm
      .confirm({
        title: 'Reject Subscription Request',
        target: `${project} asks to use ${cache}`,
        consequence: "The project can't use this cache. The project can request access later.",
        confirmLabel: 'Deny',
        failure: "Couldn't deny subscription request",
        action: () => this.caches.denySubscriptionRequest(request.cache, request.project),
      })
      .subscribe(() => {
        this.notify.success('Subscription request denied');
        this.removeDecidedSubscriptionRequestRowFromStoredPendingDecisions(request);
      });
  }

  getTeamGrantRequestTargetLink(request: TeamGrantRequestInboxEntry): UrlTree {
    return request.kind === 'cache' ? links.cache(request.target) : links.project(request.target);
  }

  getTeamGrantRequestRoleAndWorkerText(request: TeamGrantRequestInboxEntry): string {
    return [
      request.users && request.role ? `Members get ${request.role}` : null,
      request.workers ? 'Team workers' : null,
    ].filter((part) => part).join(' · ');
  }

  approveTeamGrantRequestAndRemoveRow(request: TeamGrantRequestInboxEntry): void {
    this.requestKeyWithDecisionInProgress.set(request.id);
    this.teams.approveRequest(request.team, request.id).subscribe({
      next: () => {
        this.requestKeyWithDecisionInProgress.set(null);
        this.notify.success('Request approved for the team grant');
        this.removeApprovedOrDeniedTeamGrantRequestFromPendingDecisions(request);
      },
      error: (err: unknown) => {
        this.requestKeyWithDecisionInProgress.set(null);
        this.notify.failure("Couldn't approve grant request for team", err);
      },
    });
  }

  confirmAndDenyTeamGrantRequestAndRemoveItsRow(request: TeamGrantRequestInboxEntry): void {
    const target = formatEntityLabel({ display_name: request.display_name, name: request.target });
    const team = formatEntityLabel({ display_name: request.team_display_name, name: request.team });
    this.confirm
      .confirm({
        title: 'Reject the grant request from the team',
        target: `Access Request from ${target} to ${team}`,
        consequence: 'Delete the request. Team members receive no grant.',
        confirmLabel: 'Deny',
        failure: "Couldn't decline grant request for the team",
        action: () => this.teams.denyRequest(request.team, request.id),
      })
      .subscribe(() => {
        this.notify.success('Denied the team grant request');
        this.removeApprovedOrDeniedTeamGrantRequestFromPendingDecisions(request);
      });
  }

  private removeDecidedInviteFromStoredPendingDecisions(token: string): void {
    this.pending.removeHandledItemsFromPendingDecisions((all) => ({
      ...all,
      invites: all.invites.filter((invite) => invite.token !== token),
    }));
  }

  private removeDecidedSubscriptionRequestRowFromStoredPendingDecisions(request: SubscriptionRequestItem): void {
    this.pending.removeHandledItemsFromPendingDecisions((all) => ({
      ...all,
      subscription_requests: all.subscription_requests.filter((item) => item.cache !== request.cache || item.project !== request.project),
    }));
  }

  private removeApprovedOrDeniedTeamGrantRequestFromPendingDecisions(request: TeamGrantRequestInboxEntry): void {
    this.pending.removeHandledItemsFromPendingDecisions((all) => ({
      ...all,
      team_requests: all.team_requests.filter((item) => item.id !== request.id),
    }));
  }
}
