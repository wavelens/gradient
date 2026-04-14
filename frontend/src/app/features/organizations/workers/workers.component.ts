/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { WorkersService } from '@core/services/workers.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { GradientCapabilities, Worker, WorkerRegistration } from '@core/models';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';

@Component({
  selector: 'app-workers',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    LoadingSpinnerComponent,
  ],
  templateUrl: './workers.component.html',
  styleUrl: './workers.component.scss',
})
export class WorkersComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private workersService = inject(WorkersService);
  private orgsService = inject(OrganizationsService);

  readonly capLabels: { key: keyof GradientCapabilities; label: string }[] = [
    { key: 'federate', label: 'federate' },
    { key: 'fetch',    label: 'fetch' },
    { key: 'eval',     label: 'eval' },
    { key: 'build',    label: 'build' },
    { key: 'sign',     label: 'sign' },
  ];

  loading = signal(true);
  registering = signal(false);
  deletingId = signal<string | null>(null);
  showRegisterDialog = signal(false);
  showTokenDialog = signal(false);
  errorMessage = signal<string | null>(null);

  orgName = '';
  /** The org UUID — shown as peer_id in the register dialog. */
  orgId = signal<string>('');
  workers = signal<Worker[]>([]);
  newWorkerId = '';
  newWorkerUrl = '';
  newWorkerToken = '';
  lastRegistration = signal<WorkerRegistration | null>(null);
  tokenCopied = signal(false);
  peerIdCopied = signal(false);

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.loadOrgId();
    this.loadWorkers();
  }

  private loadOrgId(): void {
    this.orgsService.getOrganization(this.orgName).subscribe({
      next: (org) => this.orgId.set(org.id),
      error: () => {},
    });
  }

  loadWorkers(): void {
    this.loading.set(true);
    this.workersService.getWorkers(this.orgName).subscribe({
      next: (workers) => {
        this.workers.set(workers);
        this.loading.set(false);
      },
      error: (err) => {
        console.error('Failed to load workers:', err);
        this.loading.set(false);
      },
    });
  }

  openRegisterDialog(): void {
    this.newWorkerId = '';
    this.newWorkerUrl = '';
    this.newWorkerToken = '';
    this.errorMessage.set(null);
    this.showRegisterDialog.set(true);
  }

  registerWorker(): void {
    if (!this.newWorkerId.trim()) return;
    this.registering.set(true);
    this.errorMessage.set(null);
    const url = this.newWorkerUrl.trim() || undefined;
    const token = this.newWorkerToken.trim() || undefined;
    this.workersService.registerWorker(this.orgName, this.newWorkerId.trim(), url, token).subscribe({
      next: (reg) => {
        this.registering.set(false);
        this.showRegisterDialog.set(false);
        // Only show the token dialog when there is something to display.
        if (reg.token || this.newWorkerToken.trim()) {
          this.lastRegistration.set(reg);
          this.tokenCopied.set(false);
          this.showTokenDialog.set(true);
        }
        this.loadWorkers();
      },
      error: (err) => {
        this.errorMessage.set(err.message || 'Failed to register worker.');
        this.registering.set(false);
      },
    });
  }

  deleteWorker(worker: Worker): void {
    this.deletingId.set(worker.worker_id);
    this.workersService.deleteWorker(this.orgName, worker.worker_id).subscribe({
      next: () => {
        this.deletingId.set(null);
        this.loadWorkers();
      },
      error: (err) => {
        console.error('Failed to delete worker:', err);
        this.deletingId.set(null);
      },
    });
  }

  copyToken(): void {
    const token = this.lastRegistration()?.token;
    if (token) {
      navigator.clipboard.writeText(token).then(() => {
        this.tokenCopied.set(true);
        setTimeout(() => this.tokenCopied.set(false), 2000);
      });
    }
  }

  copyPeerId(): void {
    const peerId = this.orgId();
    if (peerId) {
      navigator.clipboard.writeText(peerId).then(() => {
        this.peerIdCopied.set(true);
        setTimeout(() => this.peerIdCopied.set(false), 2000);
      });
    }
  }

  closeTokenDialog(): void {
    this.showTokenDialog.set(false);
    this.lastRegistration.set(null);
  }
}
