/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, booleanAttribute, computed, effect, inject, input, signal, untracked } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { RouterLink } from '@angular/router';
import type { Observable, Subscription } from 'rxjs';
import { BadgeComponent, type BadgeSeverity, ButtonComponent, LoadStateComponent, RowComponent } from '@gradient/ui/ui';
import type { Permission, ProjectReadinessResponse, TaskRepositoryFetchStatus, TeamReadinessEntry } from '@core/models';
import { accessTo } from '@core/models/access.model';
import { NotifyService } from '@core/services/notify.service';
import { ProjectsService } from '@core/services/projects.service';
import { TeamsService } from '@core/services/teams.service';
import { ManagedDisableDirective } from '@shared/access/managed-disable.directive';
import { requestIfProjectMember } from '@shared/access/request-if-member';
import { WritableDirective } from '@shared/access/writable.directive';
import { links } from '@shared/routing';
import { formatEntityLabel } from '../entity-name/entity-name';
import { EntityNameComponent } from '../entity-name/entity-name.component';

const badgeLabelAndSeverityByGitAccessStatus: Record<TaskRepositoryFetchStatus, { label: string; severity: BadgeSeverity }> = {
  verified: { label: 'Verified', severity: 'success' },
  unchecked: { label: 'Unchecked', severity: 'neutral' },
  failing: { label: 'Failed', severity: 'danger' },
};

@Component({
  selector: 'gr-project-readiness-card',
  standalone: true,
  imports: [RouterLink, BadgeComponent, ButtonComponent, LoadStateComponent, RowComponent, EntityNameComponent, WritableDirective, ManagedDisableDirective],
  template: `
    <gr-load-state flat [loading]="!readiness() && !readinessLoadError()" [error]="readinessLoadError()"
      loadingMessage="Checking project readiness..." errorTitle="Couldn't load readiness" (retry)="load()">
      @if (readiness(); as r) {
        <div class="project-readiness-rows">
          <gr-row data-testid="readiness-workers">
            Workers
            <gr-badge [severity]="r.workers.ready ? 'success' : 'warning'">{{ r.workers.ready ? 'Ready' : 'Missing' }}</gr-badge>
            <ng-container slot="meta">
              @for (team of r.workers.teams; track team.name) {
                <span>{{ team.workers }} from <gr-entity-name kind="team" [displayName]="team.display_name" [name]="team.name" /> (grant with workers)</span>
              }
              @if (r.workers.registered > 0) {
                <span>{{ r.workers.registered }} registered</span>
              }
              @if (r.workers.teams.length === 0 && r.workers.registered === 0) {
                <span>No workers</span>
              }
              @if (!r.workers.ready) {
                @for (team of r.workers.teams_granted_without_workers; track team.name) {
                  <span><gr-entity-name kind="team" [displayName]="team.display_name" [name]="team.name" /> has a grant without workers</span>
                }
              }
            </ng-container>
            <ng-container slot="actions" *appWritable="workerFixesAccessState()">
              @if (!r.workers.ready) {
                @for (team of r.workers.teams_granted_without_workers; track team.name) {
                  <button grButton size="small" severity="secondary" label="Enable workers" [attr.aria-label]="'Enable workers for ' + formatEntityLabel(team)"
                    [appManagedDisable]="workerFixesAccessState()" [disabled]="teamNameWithGrantRequestInFlight() !== null"
                    [loading]="teamNameWithGrantRequestInFlight() === team.name" (click)="addTeamWorkersToProjectTeamGrantAndReload(team)"></button>
                }
                @for (team of r.workers.teams_you_can_grant; track team.name) {
                  <button grButton size="small" severity="secondary" [label]="'Grant to ' + team.display_name + ' including workers'"
                    [attr.aria-label]="'Grant to ' + formatEntityLabel(team) + ' including workers'"
                    [appManagedDisable]="workerFixesAccessState()" [disabled]="teamNameWithGrantRequestInFlight() !== null"
                    [loading]="teamNameWithGrantRequestInFlight() === team.name" (click)="createTeamGrantForTeamWorkersOnly(team)"></button>
                }
                <a grButton size="small" severity="secondary" label="Register worker" icon="construction" [routerLink]="links.projectPage(project(), 'workers')"></a>
              }
            </ng-container>
          </gr-row>

          <gr-row data-testid="readiness-cache">
            Cache
            <gr-badge [severity]="r.cache.ready ? 'success' : 'warning'">{{ r.cache.ready ? 'Ready' : 'Missing' }}</gr-badge>
            <ng-container slot="meta">
              @for (cache of activeCacheSubscriptionsThatCanWrite(); track cache.id) {
                <gr-entity-name kind="cache" [displayName]="cache.display_name" [name]="cache.name" />
              } @empty {
                <span>None writable</span>
              }
              @for (cache of pendingWritableCacheSubscriptionRequests(); track cache.id) {
                <span>Approval pending for <gr-entity-name kind="cache" [displayName]="cache.display_name" [name]="cache.name" /></span>
              }
            </ng-container>
            <ng-container slot="actions" *appWritable="canManageSubscriptions()">
              @if (!r.cache.ready) {
                <a grButton size="small" severity="secondary" label="Subscribe to cache" icon="database" [routerLink]="links.projectPage(project(), 'caches')"></a>
              }
            </ng-container>
          </gr-row>

          <gr-row data-testid="readiness-git">
            Git Access
            <gr-badge [severity]="gitAccessBadge().severity">{{ gitAccessBadge().label }}</gr-badge>
            <ng-container slot="meta">
              @if (r.repository.tasks.length === 0) {
                <span>No repository connected yet</span>
              }
              @for (task of tasksWithFailingRepositoryAccess(); track task.name) {
                <gr-entity-name kind="task" [displayName]="task.display_name" [name]="task.name" />
                @if (task.error) {
                  <span>{{ task.error }}</span>
                }
              }
            </ng-container>
            <ng-container slot="actions" *appWritable="taskRepositoryPageLinkAccessState()">
              @for (task of tasksWithFailingRepositoryAccess(); track task.name) {
                <a grButton size="small" severity="secondary" label="Open repository" [attr.aria-label]="'Open the repository for ' + formatEntityLabel(task)"
                  [routerLink]="links.taskPage(project(), task.name, 'repository')"></a>
              }
            </ng-container>
          </gr-row>
        </div>
      }
    </gr-load-state>
  `,
  styleUrl: './project-readiness-card.component.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ProjectReadinessCardComponent {
  protected readonly links = links;
  protected readonly formatEntityLabel = formatEntityLabel;
  private readonly projects = inject(ProjectsService);
  private readonly teams = inject(TeamsService);
  private readonly notify = inject(NotifyService);
  private readonly destroyRef = inject(DestroyRef);
  private readinessRequest?: Subscription;

  readonly project = input.required<string>();
  readonly projectPermissions = input.required<readonly Permission[]>();
  readonly projectSettingsAreManagedByServer = input(false, { transform: booleanAttribute });

  protected readonly readiness = signal<ProjectReadinessResponse | null>(null);
  protected readonly readinessLoadError = signal<unknown>(null);
  protected readonly teamNameWithGrantRequestInFlight = signal<string | null>(null);

  private readonly accessHolder = computed(() => ({ managed: this.projectSettingsAreManagedByServer(), permissions: this.projectPermissions() }));
  protected readonly workerFixesAccessState = computed(() => accessTo(this.accessHolder(), 'manageWorkers'));
  protected readonly canManageSubscriptions = computed(() => accessTo(this.accessHolder(), 'manageSubscriptions'));
  protected readonly taskRepositoryPageLinkAccessState = computed(() => accessTo(this.accessHolder(), 'editTask'));

  protected readonly activeCacheSubscriptionsThatCanWrite = computed(() =>
    (this.readiness()?.cache.subscriptions ?? []).filter((cache) => cache.status === 'active' && cache.mode !== 'ReadOnly'),
  );
  protected readonly pendingWritableCacheSubscriptionRequests = computed(() =>
    (this.readiness()?.cache.subscriptions ?? []).filter((cache) => cache.status === 'pending' && cache.mode !== 'ReadOnly'),
  );
  protected readonly tasksWithFailingRepositoryAccess = computed(() =>
    (this.readiness()?.repository.tasks ?? []).filter((task) => task.status === 'failing'),
  );
  protected readonly gitAccessBadge = computed(() => badgeLabelAndSeverityByGitAccessStatus[this.readiness()?.repository.status ?? 'unchecked']);

  constructor() {
    effect(() => {
      this.project();
      this.projectPermissions();
      untracked(() => this.load());
    });
  }

  load(): void {
    this.readinessRequest?.unsubscribe();
    this.readinessLoadError.set(null);
    this.readinessRequest = requestIfProjectMember({ permissions: [...this.projectPermissions()] }, () => this.projects.getProjectReadiness(this.project()))
      .pipe(takeUntilDestroyed(this.destroyRef))
      .subscribe({
        next: (readiness) => this.readiness.set(readiness),
        error: (error: unknown) => {
          this.readiness.set(null);
          this.readinessLoadError.set(error);
        },
      });
  }

  protected addTeamWorkersToProjectTeamGrantAndReload(team: TeamReadinessEntry): void {
    this.sendTeamGrantChangeRequestAndReloadReadiness(
      team,
      this.teams.updateProjectGrant(this.project(), team.name, { workers: true }),
      () => 'Workers enabled',
      "Couldn't enable worker",
    );
  }

  protected createTeamGrantForTeamWorkersOnly(team: TeamReadinessEntry): void {
    this.sendTeamGrantChangeRequestAndReloadReadiness(
      team,
      this.teams.grantProject(this.project(), { team: team.name, users: false, workers: true }),
      (message) => message,
      "Couldn't grant team",
    );
  }

  private sendTeamGrantChangeRequestAndReloadReadiness(
    team: TeamReadinessEntry,
    request: Observable<string>,
    success: (message: string) => string,
    failure: string,
  ): void {
    if (this.teamNameWithGrantRequestInFlight() !== null) return;
    this.teamNameWithGrantRequestInFlight.set(team.name);
    request.pipe(takeUntilDestroyed(this.destroyRef)).subscribe({
      next: (message) => {
        this.teamNameWithGrantRequestInFlight.set(null);
        this.notify.success(success(message));
        this.load();
      },
      error: (error: unknown) => {
        this.teamNameWithGrantRequestInFlight.set(null);
        this.notify.failure(failure, error);
      },
    });
  }
}
