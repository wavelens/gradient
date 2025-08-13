# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.conf import settings

def global_variables(request):
    return {
        'success' : 'waiting',
        'email_enabled': settings.GRADIENT_EMAIL_ENABLED,
        'email_require_verification': settings.GRADIENT_EMAIL_REQUIRE_VERIFICATION,
        'disable_registration': settings.GRADIENT_DISABLE_REGISTRATION,
        'oidc_required': settings.GRADIENT_OIDC_REQUIRED,
    }
