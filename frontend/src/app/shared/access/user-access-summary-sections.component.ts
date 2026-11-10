/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { LoadStateComponent, MessageBannerComponent, RowComponent, RowListComponent, SettingsSectionComponent } from '@gradient/ui/ui';
import type { UserAccessSummary, UserAccessibleProjectOrCacheRow, UserTeamMembership } from '@core/models';
import { entityKindToIconName } from '@shared/ui';
import { links } from '@shared/routing';
import { getAccessReasonLabel } from '@shared/members-page/access-reason-label';
import { teamMemberSourceLabels } from '@shared/members-page/members-page-requests';
import { teamRoleValueToDisplayLabel } from './team-access';

@Component({
  selector: 'app-user-access-summary-sections',
  standalone: true,
  imports: [LoadStateComponent, MessageBannerComponent, RowComponent, RowListComponent, SettingsSectionComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    @let access = summary();
    @if (access.superuser) {
      <gr-message-banner type="info">This account is an instance administrator and can open all projects, caches and teams.</gr-message-banner>
    }
    <gr-load-state
      [empty]="isEmpty()"
      emptyIcon="lock"
      emptyTitle="No access yet"
      emptyMessage="Accept an invitation, or ask a project, cache or team admin to add this account."
    >
      @if (access.projects.length) {
        <gr-settings-section title="Projects" [card]="false">
          <gr-row-list>
            @for (project of access.projects; track project.name) {
              <gr-row [icon]="entityKindToIconName.project" [link]="links.project(project.name)" [linkLabel]="'Open project ' + project.display_name">
                {{ project.display_name }}
                <span slot="meta">{{ describeProjectOrCacheAccess(project) }}</span>
              </gr-row>
            }
          </gr-row-list>
        </gr-settings-section>
      }
      @if (access.caches.length) {
        <gr-settings-section title="Caches" [card]="false">
          <gr-row-list>
            @for (cache of access.caches; track cache.name) {
              <gr-row [icon]="entityKindToIconName.cache" [link]="links.cache(cache.name)" [linkLabel]="'Open cache ' + cache.display_name">
                {{ cache.display_name }}
                <span slot="meta">{{ describeProjectOrCacheAccess(cache) }}</span>
              </gr-row>
            }
          </gr-row-list>
        </gr-settings-section>
      }
      @if (access.teams.length) {
        <gr-settings-section title="Teams" [card]="false">
          <gr-row-list>
            @for (team of access.teams; track team.name) {
              <gr-row [icon]="entityKindToIconName.team" [link]="links.team(team.name)" [linkLabel]="'Open team ' + team.display_name">
                {{ team.display_name }}
                <span slot="meta">{{ describeTeamMembership(team) }}</span>
              </gr-row>
            }
          </gr-row-list>
        </gr-settings-section>
      }
    </gr-load-state>
  `,
})
export class UserAccessSummarySectionsComponent {
  protected readonly entityKindToIconName = entityKindToIconName;
  protected readonly links = links;

  summary = input.required<UserAccessSummary>();

  protected isEmpty = computed(() => {
    const { projects, caches, teams } = this.summary();
    return projects.length + caches.length + teams.length === 0;
  });

  protected describeProjectOrCacheAccess(row: UserAccessibleProjectOrCacheRow): string {
    return [...(row.role ? [`Role ${row.role}`] : []), ...row.reasons.map(getAccessReasonLabel)].join(' · ');
  }

  protected describeTeamMembership(team: UserTeamMembership): string {
    return `Role ${teamRoleValueToDisplayLabel(team.role)} · ${teamMemberSourceLabels[team.source]}`;
  }
}
