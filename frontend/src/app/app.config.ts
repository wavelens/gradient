/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ApplicationConfig, APP_INITIALIZER, provideBrowserGlobalErrorListeners, inject } from '@angular/core';
import { provideRouter, TitleStrategy, withRouterConfig } from '@angular/router';
import { provideHttpClient, withInterceptors, withXhr } from '@angular/common/http';
import { provideAnimations } from '@angular/platform-browser/animations';

import { routes } from './app.routes';
import { authInterceptor } from '@core/interceptors/auth.interceptor';
import { errorInterceptor } from '@core/interceptors/error.interceptor';
import { ConfigService } from '@core/services/config.service';
import { GradientTitleStrategy } from '@core/title/gradient-title-strategy';

export const appConfig: ApplicationConfig = {
  providers: [
    provideBrowserGlobalErrorListeners(),
    provideRouter(routes, withRouterConfig({ paramsInheritanceStrategy: 'always' })),
    { provide: TitleStrategy, useClass: GradientTitleStrategy },
    provideHttpClient(withXhr(), 
      withInterceptors([authInterceptor, errorInterceptor])
    ),
    provideAnimations(),
    {
      provide: APP_INITIALIZER,
      useFactory: () => {
        const configService = inject(ConfigService);
        return () => configService.load();
      },
      multi: true,
    },
  ]
};
