/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { firstValueFrom } from 'rxjs';
import { environment } from '@environments/environment';

export type CreatePermission = 'none' | 'superusers' | 'everyone';

interface ServerConfig {
  version: string;
  oidc_enabled: boolean;
  oidc_required: boolean;
  registration_enabled: boolean;
  email_verification_enabled: boolean;
  smtp_enabled: boolean;
  create_org: CreatePermission;
  create_cache: CreatePermission;
}

@Injectable({ providedIn: 'root' })
export class ConfigService {
  private http = inject(HttpClient);

  backendVersion = '';
  frontendVersion = environment.version;
  oidcEnabled = false;
  oidcRequired = false;
  registrationDisabled = false;
  emailVerificationEnabled = false;
  smtpEnabled = false;
  createOrg: CreatePermission = 'everyone';
  createCache: CreatePermission = 'everyone';

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
          this.oidcEnabled = res.message.oidc_enabled;
          this.oidcRequired = res.message.oidc_required;
          this.registrationDisabled = !res.message.registration_enabled;
          this.emailVerificationEnabled = res.message.email_verification_enabled;
          this.smtpEnabled = res.message.smtp_enabled;
          this.createOrg = res.message.create_org ?? 'everyone';
          this.createCache = res.message.create_cache ?? 'everyone';
        }
      })
      .catch(() => {
        // Keep defaults if config fetch fails
      });
  }
}
