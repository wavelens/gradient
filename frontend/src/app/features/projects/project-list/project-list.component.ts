/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { EntityNameComponent, entityKindToIconName, formatEntityLabel } from '@shared/ui';
import { Router, RouterModule } from '@angular/router';
import { ProjectsService } from '@core/services/projects.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import {
  BadgeComponent,
  EmptyStateComponent,
  LoadMoreComponent,
  ButtonComponent,
  CardGridComponent,
  LoadStateComponent,
  NavCardComponent,
  PageLayoutComponent,
} from '@gradient/ui/ui';
import { CreateProjectDialogComponent, injectCanCreateProject, openCreateDialogFromUrlIfAllowed } from '@shared/forms';
import { ProjectSummary, PublicProjectSummary } from '@core/models';
import { links } from '@shared/routing';
import { pagedList, runReloadIntervalWhileTabVisible } from '@shared/paging';
import { EMPTY, merge } from 'rxjs';
import { PHASE_SEVERITY } from '@shared/evaluation';
import { PendingInvitationsBannerComponent, getDisplayNameWithOwnerTeam } from '@shared/access';

@Component({
  selector: 'app-project-list',
  standalone: true,
  imports: [
    EntityNameComponent,
    RouterModule,
    ButtonComponent,
    LoadStateComponent,
    PageLayoutComponent,
    NavCardComponent,
    CardGridComponent,
    BadgeComponent,
    EmptyStateComponent,
    LoadMoreComponent,
    CreateProjectDialogComponent,
    PendingInvitationsBannerComponent,
  ],
  templateUrl: './project-list.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ProjectListComponent implements OnInit {
  protected readonly entityKindToIconName = entityKindToIconName;
  protected readonly formatEntityLabel = formatEntityLabel;
  protected readonly links = links;
  protected readonly phaseSeverity = PHASE_SEVERITY;
  protected readonly getDisplayNameWithOwnerTeam = getDisplayNameWithOwnerTeam;

  private projectsService = inject(ProjectsService);
  protected authService = inject(AuthService);
  private config = inject(ConfigService);
  private router = inject(Router);

  protected canCreateProject = injectCanCreateProject();

  projects = pagedList<ProjectSummary>('projects', (page) => this.projectsService.getProjects(page, 50));
  showCreateDialog = signal(false);

  publicProjects = pagedList<PublicProjectSummary>('public projects', (page) => this.projectsService.getPublicProjects(page, 50));

  constructor() {
    runReloadIntervalWhileTabVisible(() => merge(
      this.authService.isAuthenticated() ? this.projects.refreshFirstPageSilently() : EMPTY,
      this.config.publicListing ? this.publicProjects.refreshFirstPageSilently() : EMPTY,
    ));
    openCreateDialogFromUrlIfAllowed(() => this.canCreateProject(), () => this.openCreateDialog());
  }

  ngOnInit(): void {
    if (this.authService.isAuthenticated()) this.projects.reload();
    if (this.config.publicListing) this.publicProjects.reload();
  }

  openCreateDialog(): void {
    this.showCreateDialog.set(true);
  }

  openCreatedProject(name: string): void {
    this.router.navigateByUrl(links.project(name));
  }

  get filteredPublicProjects(): PublicProjectSummary[] {
    const ownedIds = new Set(this.projects.items().map((o) => o.id));
    return this.publicProjects.items().filter((o) => !ownedIds.has(o.id));
  }
}
