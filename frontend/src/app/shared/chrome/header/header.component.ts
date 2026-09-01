/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, inject, ChangeDetectionStrategy } from '@angular/core';
import { IconComponent } from '@shared/ui';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';

@Component({
  selector: 'app-header',
  standalone: true,
  imports: [IconComponent, CommonModule, RouterModule],
  templateUrl: './header.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './header.component.scss',
})
export class HeaderComponent {
  authService = inject(AuthService);
  private config = inject(ConfigService);

  get registrationDisabled() { return this.config.registrationDisabled; }

  logout(): void {
    this.authService.logout().subscribe();
  }
}
