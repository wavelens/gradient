/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { EvaluationsService, isHtmlArtefact } from '@core/services/evaluations.service';
import { AuthService } from '@core/services/auth.service';
import { ProjectsService } from '@core/services/projects.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { type BuildOrigin, buildOriginLink, readBuildOrigin } from '@shared/routing';
import {
  BadgeComponent,
  ButtonComponent,
  EmptyStateComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';
import { environment } from '@environments/environment';
import { formatBytes, packageNameFromDerivationPath } from '@shared/text';
import { BuildProduct } from '@core/models';

/** Readable labels for product types and subtypes from Hydra. A more specific subtype can take priority. */
const ARTEFACT_TYPE_LABELS: Record<string, string> = {
  'binary-dist': 'Binary',
  'source-dist': 'Source',
  manual: 'Manual',
  readme: 'Readme',
  pdf: 'PDF',
  report: 'Report',
  coverage: 'Coverage',
  'nix-build': 'Nix build',
  doc: 'Document',
  file: 'File',
};

@Component({
  selector: 'app-build-artefacts',
  standalone: true,
  imports: [RouterModule, LoadStateComponent, PageLayoutComponent, EmptyStateComponent, BadgeComponent, ButtonComponent, RowListComponent, RowComponent],
  templateUrl: './build-artefacts.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './build-artefacts.component.scss',
})
export class BuildArtefactsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private crumbs = inject(BreadcrumbsService);
  private evalService = inject(EvaluationsService);
  private authService = inject(AuthService);
  private projectsService = inject(ProjectsService);

  loading = signal(true);
  loadError = signal<unknown>(null);
  artefacts = signal<BuildProduct[]>([]);
  packageName = signal('');
  private downloadToken = signal<string | null>(null);

  projectName = '';
  buildId = '';
  private from = signal<BuildOrigin>({});
  breadcrumb = computed(() => this.crumbs.buildPage(this.projectName, this.from(), this.buildId, 'Artefacts'));
  back = computed(() => buildOriginLink(this.projectName, this.from(), this.buildId));

  ngOnInit(): void {
    this.projectName     = this.route.snapshot.paramMap.get('project') || '';
    this.buildId     = this.route.snapshot.paramMap.get('buildId') || '';
    this.from.set(readBuildOrigin(this.route.snapshot.queryParamMap));
    this.loadArtefacts();
    this.evalService.getBuild(this.buildId).subscribe({
      next: (build) => {
        this.packageName.set(packageNameFromDerivationPath(build.derivation_path));
        this.from.update((from) => (from.task || from.evaluation ? from : { evaluation: build.evaluation }));
      },
      error: () => this.packageName.set(this.buildId.slice(0, 8)),
    });
    if (this.authService.isAuthenticated()) {
      this.projectsService.getProject(this.projectName).subscribe({
        next: (project) => {
          if (!project.public) {
            this.evalService.getDownloadToken(this.buildId).subscribe({
              next: (token) => this.downloadToken.set(token),
            });
          }
        },
      });
    }
  }

  loadArtefacts(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.evalService.getBuildDownloads(this.buildId).subscribe({
      next: (products) => {
        this.artefacts.set(products);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  downloadUrl(artefact: BuildProduct): string {
    const base = `${environment.apiUrl}/builds/${this.buildId}/download/${encodeURIComponent(artefact.name)}`;
    const token = this.downloadToken();
    return token ? `${base}?token=${encodeURIComponent(token)}` : base;
  }

  readonly formatBytes = formatBytes;

  /** True when the artefact should open inline in a browser tab. */
  isHtml(p: BuildProduct): boolean {
    return isHtmlArtefact(p);
  }

  fileTypeLabel(p: BuildProduct): string {
    if (this.isHtml(p)) return 'HTML';
    const subtype = p.subtype && p.subtype !== 'out' ? p.subtype : '';
    return ARTEFACT_TYPE_LABELS[subtype] ?? ARTEFACT_TYPE_LABELS[p.file_type ?? ''] ?? (subtype || p.file_type || 'File');
  }
}
