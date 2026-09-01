/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Routes } from '@angular/router';
import { StyleguideShellComponent } from './styleguide-shell.component';

export const STYLEGUIDE_ROUTES: Routes = [
  {
    path: '',
    component: StyleguideShellComponent,
    children: [
      { path: '', title: 'Style Guide', loadComponent: () => import('./pages/overview.component').then((m) => m.OverviewComponent) },
      { path: 'foundations', title: 'Foundations', loadComponent: () => import('./pages/foundations.component').then((m) => m.FoundationsComponent) },
      { path: 'elements', title: 'Elements', loadComponent: () => import('./pages/elements.component').then((m) => m.ElementsComponent) },
      { path: 'components', title: 'Components', loadComponent: () => import('./pages/components.component').then((m) => m.ComponentsComponent) },
      { path: 'patterns', title: 'Patterns', loadComponent: () => import('./pages/patterns.component').then((m) => m.PatternsComponent) },
    ],
  },
];
