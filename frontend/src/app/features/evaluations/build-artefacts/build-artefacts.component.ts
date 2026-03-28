/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { EvaluationsService, BuildProduct } from '@core/services/evaluations.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { environment } from '@environments/environment';

@Component({
  selector: 'app-build-artefacts',
  standalone: true,
  imports: [CommonModule, RouterModule, LoadingSpinnerComponent],
  templateUrl: './build-artefacts.component.html',
  styleUrl: './build-artefacts.component.scss',
})
export class BuildArtefactsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private evalService = inject(EvaluationsService);

  loading = signal(true);
  downloading = signal<string | null>(null);
  artefacts = signal<BuildProduct[]>([]);

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

  async download(artefact: BuildProduct): Promise<void> {
    this.downloading.set(artefact.name);
    try {
      const token = localStorage.getItem('jwt_token') || sessionStorage.getItem('jwt_token') || '';
      const url = `${environment.apiUrl}/builds/${this.buildId}/download/${encodeURIComponent(artefact.name)}`;
      const response = await fetch(url, {
        headers: token ? { Authorization: `Bearer ${token}` } : {},
      });
      if (!response.ok) throw new Error('Download failed');
      const blob = await response.blob();
      const objectUrl = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = objectUrl;
      a.download = artefact.name;
      a.click();
      URL.revokeObjectURL(objectUrl);
    } catch {
      // ignore
    } finally {
      this.downloading.set(null);
    }
  }

  goBack(): void {
    if (this.projectName) this.router.navigate(['/organization', this.orgName, 'project', this.projectName]);
    else if (this.evalId) this.router.navigate(['/organization', this.orgName, 'log', this.evalId]);
    else this.router.navigate(['/organization', this.orgName]);
  }

  buildShortId(): string {
    return this.buildId.slice(0, 8);
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
