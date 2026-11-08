/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ApplicationConfig, APP_INITIALIZER, provideBrowserGlobalErrorListeners, inject } from '@angular/core';
import {
  RedirectCommand,
  RouteReuseStrategy,
  Router,
  provideRouter,
  TitleStrategy,
  withInMemoryScrolling,
  withNavigationErrorHandler,
  withRouterConfig,
} from '@angular/router';
import { provideHttpClient, withInterceptors, withXhr } from '@angular/common/http';

import { routes } from './app.routes';
import { authInterceptor } from '@core/interceptors/auth.interceptor';
import { errorInterceptor } from '@core/interceptors/error.interceptor';
import { ConfigService } from '@core/services/config.service';
import { GradientTitleStrategy } from '@core/title/gradient-title-strategy';
import { ParamReuseStrategy } from '@core/routing/param-reuse-strategy';
import { provideScrollToTopOnPageChange } from '@core/routing/scroll-to-top';
import { ApiError } from '@core/services/api.service';

export const appConfig: ApplicationConfig = {
  providers: [
    provideBrowserGlobalErrorListeners(),
    provideRouter(
      routes,
      withRouterConfig({ paramsInheritanceStrategy: 'always' }),
      withInMemoryScrolling(),
      withNavigationErrorHandler(({ error, url }) => {
        if (error instanceof ApiError && error.unreachable) return;
        const router = inject(Router);
        return new RedirectCommand(router.createUrlTree(['/error/500'], { queryParams: { from: url } }), { browserUrl: url });
      }),
    ),
    provideScrollToTopOnPageChange(),
    { provide: TitleStrategy, useClass: GradientTitleStrategy },
    { provide: RouteReuseStrategy, useClass: ParamReuseStrategy },
    provideHttpClient(withXhr(), 
      withInterceptors([authInterceptor, errorInterceptor])
    ),
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
