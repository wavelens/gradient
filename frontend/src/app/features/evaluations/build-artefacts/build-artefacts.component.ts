/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { EvaluationsService, BuildProduct } from '@core/services/evaluations.service';
import { AuthService } from '@core/services/auth.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { environment } from '@environments/environment';

@Component({
  selector: 'app-build-artefacts',
  standalone: true,
  imports: [RouterModule, LoadingSpinnerComponent],
  templateUrl: './build-artefacts.component.html',
  styleUrl: './build-artefacts.component.scss',
})
export class BuildArtefactsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private evalService = inject(EvaluationsService);
  private authService = inject(AuthService);
  private orgsService = inject(OrganizationsService);

  loading = signal(true);
  artefacts = signal<BuildProduct[]>([]);
  private downloadToken = signal<string | null>(null);

  orgName = '';
  buildId = '';
  private projectName = '';
  private evalId = '';

  ngOnInit(): void {
    this.orgName     = this.route.snapshot.paramMap.get('org') || '';
    this.buildId     = this.route.snapshot.paramMap.get('buildId') || '';
    this.projectName = this.route.snapshot.queryParamMap.get('project') || '';
    this.evalId      = this.route.snapshot.queryParamMap.get('evalId') || '';
    this.loadArtefacts();
    if (this.authService.isAuthenticated()) {
      this.orgsService.getOrganization(this.orgName).subscribe({
        next: (org) => {
          if (!org.public) {
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
    if (this.projectName) this.router.navigate(['/organization', this.orgName, 'project', this.projectName]);
    else if (this.evalId) this.router.navigate(['/organization', this.orgName, 'log', this.evalId]);
    else this.router.navigate(['/organization', this.orgName]);
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

  fileTypeLabel(fileType: string): string {
    const labels: Record<string, string> = {
      'iso': 'ISO',
      'tar': 'TAR',
      'rpm': 'RPM',
      'deb': 'DEB',
      'doc': 'DOC',
      'file': 'FILE',
    };
    return labels[fileType] ?? fileType.toUpperCase();
  }
}
