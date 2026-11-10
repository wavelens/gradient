/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, inject, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, Router } from '@angular/router';
import {
  AutoCompleteComponent,
  ButtonComponent,
  FormFieldComponent,
  LoadStateComponent,
  PageLayoutComponent,
  type AutoCompleteSuggestion,
} from '@gradient/ui/ui';
import type { RevokedUserAccessCounts, UserAccessSummary } from '@core/models';
import { AdminService } from '@core/services/admin.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { NotifyService } from '@core/services/notify.service';
import { UserService } from '@core/services/user.service';
import { formatMemberNameForAutocompleteSuggestion } from '@shared/access';
import { UserAccessSummarySectionsComponent } from '@shared/access/user-access-summary-sections.component';
import { ConfirmService } from '@shared/confirm';
import { links } from '@shared/routing';
import { plural } from '@shared/text';

export function formatRevokedAccessCountsText(counts: RevokedUserAccessCounts): string {
  return [
    plural(counts.projects, 'project'),
    plural(counts.caches, 'cache'),
    plural(counts.teams, 'team'),
    plural(counts.invitations, 'invitation'),
  ].join(', ');
}

@Component({
  selector: 'app-admin-user-access',
  standalone: true,
  imports: [
    FormsModule,
    AutoCompleteComponent,
    ButtonComponent,
    FormFieldComponent,
    LoadStateComponent,
    PageLayoutComponent,
    UserAccessSummarySectionsComponent,
  ],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <gr-page-layout
      width="settings"
      [breadcrumb]="breadcrumb()"
      [title]="username() || 'User access'"
      subtitle="Enter a username to show all projects, caches and teams that the user can access."
    >
      @if (summary() && username()) {
        <button slot="actions" grButton label="Revoke All Access" icon="person_remove" severity="danger" (click)="confirmAndRevokeAllUserAccess()"></button>
      }
      <gr-form-field label="Username" for="admin-user-access-username">
        <gr-autocomplete
          inputId="admin-user-access-username"
          [ngModel]="typedUsername()"
          (ngModelChange)="typedUsername.set($event)"
          [suggestions]="suggestions()"
          (completeMethod)="search($event)"
          [forceSelection]="false"
          (enter)="navigateToAccessPageForTypedUsername()"
        />
      </gr-form-field>
      <button grButton label="View access" icon="search" severity="secondary" (click)="navigateToAccessPageForTypedUsername()"></button>
      @if (username()) {
        <gr-load-state [loading]="loading()" [error]="loadError()" loadingMessage="Loading access..." errorTitle="Couldn't load access" (retry)="load()">
          @if (summary(); as summary) {
            <app-user-access-summary-sections [summary]="summary" />
          }
        </gr-load-state>
      }
    </gr-page-layout>
  `,
})
export class AdminUserAccessComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private destroyRef = inject(DestroyRef);
  private crumbs = inject(BreadcrumbsService);
  private admin = inject(AdminService);
  private users = inject(UserService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  username = signal('');
  typedUsername = signal('');
  suggestions = signal<AutoCompleteSuggestion[]>([]);
  summary = signal<UserAccessSummary | null>(null);
  loading = signal(false);
  loadError = signal<unknown>(null);

  breadcrumb = computed(() => {
    const user = this.username();
    return user
      ? this.crumbs.admin({ label: 'User access', link: links.userAccessPageUrl() }, { label: user })
      : this.crumbs.admin({ label: 'User access' });
  });

  ngOnInit(): void {
    this.route.paramMap.pipe(takeUntilDestroyed(this.destroyRef)).subscribe((params) => {
      const user = params.get('username') ?? '';
      this.username.set(user);
      this.typedUsername.set(user);
      this.summary.set(null);
      if (user) this.load();
    });
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.admin.getUserAccessToAllProjectsCachesAndTeams(this.username()).subscribe({
      next: (summary) => {
        this.summary.set(summary);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  search(event: { query: string }): void {
    if (!event.query.trim()) {
      this.suggestions.set([]);
      return;
    }
    this.users.searchUsers(event.query).subscribe({
      next: (found) => this.suggestions.set(found.map(formatMemberNameForAutocompleteSuggestion)),
      error: () => this.suggestions.set([]),
    });
  }

  navigateToAccessPageForTypedUsername(): void {
    const user = this.typedUsername().trim();
    if (user) void this.router.navigateByUrl(links.userAccessPageUrl(user));
  }

  confirmAndRevokeAllUserAccess(): void {
    const user = this.username();
    this.confirm
      .confirm({
        title: 'Revoke All Access',
        target: user,
        consequence: `No direct memberships in projects and caches, no team memberships and no pending invitations remain for ${user}. Memberships from server configuration and identity provider groups remain.`,
        confirmLabel: 'Revoke',
        failure: "Couldn't revoke access",
        severity: 'danger',
        action: () => this.admin.removeUserMembershipsAndInvitationsAndReturnCounts(user),
      })
      .subscribe((counts) => {
        this.notify.success(`Access revoked: ${formatRevokedAccessCountsText(counts)}`);
        this.load();
      });
  }
}
