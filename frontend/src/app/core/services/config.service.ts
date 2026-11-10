/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { firstValueFrom } from 'rxjs';
import { environment } from '@environments/environment';
import { CreatePermission, LoginProviderInfo, ServerConfig } from '@core/models';

@Injectable({ providedIn: 'root' })
export class ConfigService {
  private http = inject(HttpClient);

  backendVersion = '';
  backendCommit: string | null = null;
  frontendVersion = environment.version;
  oidcEnabled = false;
  loginProviders: LoginProviderInfo[] = [];
  passwordLoginEnabled = true;
  registrationDisabled = false;
  emailVerificationEnabled = false;
  smtpEnabled = false;
  createProject: CreatePermission = 'everyone';
  createCache: CreatePermission = 'everyone';
  createTeam: CreatePermission = 'everyone';
  githubAppEnabled = false;
  sshEnabled = false;
  sshPort: number | null = null;
  gradientCiEnabled = false;
  gradientCiUrl = 'https://servers.gradient.ci';
  serveUrl = globalThis.location?.origin ?? '';
  logoUrl: string | null = null;
  execEnabled = false;
  publicListing = true;
  maxKeepEvaluationsPerTask: number | null = null;

  canCreate(permission: CreatePermission, isSuperuser: boolean): boolean {
    switch (permission) {
      case 'everyone':
        return true;
      case 'superusers':
        return isSuperuser;
      default:
        return false;
    }
  }

  load(): Promise<void> {
    return firstValueFrom(
      this.http.get<{ error: boolean; message: ServerConfig }>(
        `${environment.apiUrl}/config`
      )
    )
      .then((res) => {
        if (!res.error) {
          this.backendVersion = res.message.version;
          this.backendCommit = res.message.commit ?? null;
          this.oidcEnabled = res.message.oidc_enabled;
          this.loginProviders = res.message.login_providers;
          this.passwordLoginEnabled = res.message.password_login_enabled;
          this.registrationDisabled = !res.message.registration_enabled;
          this.emailVerificationEnabled = res.message.email_verification_enabled;
          this.smtpEnabled = res.message.smtp_enabled;
          this.createProject = res.message.create_project ?? 'everyone';
          this.createCache = res.message.create_cache ?? 'everyone';
          this.createTeam = res.message.create_team ?? 'everyone';
          this.githubAppEnabled = res.message.github_app_enabled ?? false;
          this.sshEnabled = res.message.ssh_enabled ?? false;
          this.sshPort = res.message.ssh_port ?? null;
          this.gradientCiEnabled = res.message.gradient_ci_enabled ?? false;
          this.gradientCiUrl = res.message.gradient_ci_url ?? this.gradientCiUrl;
          this.serveUrl = res.message.serve_url?.replace(/\/+$/, '') || this.serveUrl;
          this.logoUrl = res.message.logo_url ?? null;
          this.execEnabled = res.message.exec_enabled;
          this.publicListing = res.message.public_listing;
          this.maxKeepEvaluationsPerTask = res.message.max_keep_evaluations_per_task ?? null;
        }
      })
      .catch(() => {
        // Keep defaults if config fetch fails
      });
  }
}
