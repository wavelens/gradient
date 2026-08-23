/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { EvaluationsService, BuildProduct, isHtmlArtefact } from '@core/services/evaluations.service';
import { AuthService } from '@core/services/auth.service';
import { ProjectsService } from '@core/services/projects.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { environment } from '@environments/environment';

@Component({
  selector: 'app-build-artefacts',
  standalone: true,
  imports: [RouterModule, LoadingSpinnerComponent],
  templateUrl: './build-artefacts.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './build-artefacts.component.scss',
})
export class BuildArtefactsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private evalService = inject(EvaluationsService);
  private authService = inject(AuthService);
  private projectsService = inject(ProjectsService);

  loading = signal(true);
  artefacts = signal<BuildProduct[]>([]);
  private downloadToken = signal<string | null>(null);

  projectName = '';
  buildId = '';
  private taskName = '';
  private evalId = '';

  ngOnInit(): void {
    this.projectName     = this.route.snapshot.paramMap.get('project') || '';
    this.buildId     = this.route.snapshot.paramMap.get('buildId') || '';
    this.taskName = this.route.snapshot.queryParamMap.get('task') || '';
    this.evalId      = this.route.snapshot.queryParamMap.get('evalId') || '';
    this.loadArtefacts();
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
    this.evalService.getBuildDownloads(this.buildId).subscribe({
      next: (products) => {
        this.artefacts.set(products);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  downloadUrl(artefact: BuildProduct): string {
    const base = `${environment.apiUrl}/builds/${this.buildId}/download/${encodeURIComponent(artefact.name)}`;
    const token = this.downloadToken();
    return token ? `${base}?token=${encodeURIComponent(token)}` : base;
  }

  goBack(): void {
    if (this.taskName) this.router.navigate(['/project', this.projectName, 'task', this.taskName]);
    else if (this.evalId) this.router.navigate(['/project', this.projectName, 'log', this.evalId]);
    else this.router.navigate(['/project', this.projectName]);
  }

  buildShortId(): string {
    return this.buildId.slice(0, 8);
  }

  formatSize(bytes: number | undefined): string {
    if (bytes === undefined || bytes === null) return '';
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  }

  /** True when the artefact should open inline in a browser tab. */
  isHtml(p: BuildProduct): boolean {
    return isHtmlArtefact(p);
  }

  /**
   * Display label combining Hydra type and subtype. Falls back to whichever
   * field is populated.  Examples: `file html` → `HTML`, `doc readme` → `DOC/README`,
   * `nix-build out` → `NIX-BUILD`.
   */
  fileTypeLabel(p: BuildProduct): string {
    if (this.isHtml(p)) return 'HTML';
    const t = p.file_type?.toUpperCase() ?? '';
    const s = p.subtype?.toUpperCase() ?? '';
    if (t && s && s !== 'OUT') return `${t}/${s}`;
    return t || s || 'FILE';
  }
}
