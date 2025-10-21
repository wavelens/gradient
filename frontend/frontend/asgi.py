# SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

"""
ASGI config for frontend project.

It exposes the ASGI callable as a module-level variable named ``application``.

For more information on this file, see
https://docs.djangoproject.com/en/4.2/howto/deployment/asgi/
"""

import os
from django.core.asgi import get_asgi_application

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "frontend.settings")
django_asgi_app = get_asgi_application()

from channels.routing import ProtocolTypeRouter

application = ProtocolTypeRouter(
    {
        "http": django_asgi_app,
    }
)
