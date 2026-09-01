/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';

@Component({
  selector: 'app-board-layout',
  standalone: true,
  imports: [CommonModule, RouterModule],
  template: `
    <div class="board">
      <h1>Job Board</h1>
      <nav class="board-nav">
        <a routerLink="overview" routerLinkActive="active">Overview</a>
        <a routerLink="live" routerLinkActive="active">Live Jobs</a>
        <a routerLink="scheduler" routerLinkActive="active">Scheduler</a>
        <a routerLink="throughput" routerLinkActive="active">Throughput</a>
        <a routerLink="durations" routerLinkActive="active">Durations</a>
        <a routerLink="workers" routerLinkActive="active">Workers</a>
        <a routerLink="cache" routerLinkActive="active">Cache</a>
        <a routerLink="network" routerLinkActive="active">Network</a>
        <a routerLink="expensive" routerLinkActive="active">Jobs</a>
        <a routerLink="expensive-evals" routerLinkActive="active">Evals</a>
        <a routerLink="health" routerLinkActive="active">System Health</a>
      </nav>
      <router-outlet></router-outlet>
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './board-layout.component.scss',
})
export class BoardLayoutComponent {}
