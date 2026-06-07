/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
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
        <a routerLink="scheduler" routerLinkActive="active">Scheduler &amp; Scoring</a>
        <a routerLink="throughput" routerLinkActive="active">Throughput</a>
        <a routerLink="durations" routerLinkActive="active">Durations</a>
        <a routerLink="workers" routerLinkActive="active">Workers</a>
        <a routerLink="expensive" routerLinkActive="active">Expensive Jobs</a>
      </nav>
      <router-outlet></router-outlet>
    </div>
  `,
  styles: [
    `
      .board {
        padding: 1.5rem;
        max-width: 1200px;
        margin: 0 auto;
      }
      h1 {
        color: #fff;
        font-size: 1.5rem;
        margin: 0 0 1rem;
      }
      .board-nav {
        display: flex;
        gap: 1.25rem;
        margin-bottom: 1.5rem;
        border-bottom: 1px solid #2d333b;
      }
      .board-nav a {
        color: #abb0b4;
        padding: 0.5rem 0;
        text-decoration: none;
        border-bottom: 2px solid transparent;
      }
      .board-nav a.active {
        color: #fff;
        border-bottom-color: #17a2b8;
      }
    `,
  ],
})
export class BoardLayoutComponent {}
