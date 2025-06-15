# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.urls import path

from .views import *

urlpatterns = [
    path("account/login", UserLoginView.as_view(), name="login"),
    path("account/register", register, name="register"),
    path("account/logout", logout_view, name="logout"),

    path("<str:org>", workflow, name="workflow"),
    path("<str:org>/log", log, name="log"),
    path("<str:org>/log/<str:evaluation_id>", log, name="log-eval"),
    path("<str:org>/download", download, name="download"),
    path("<str:org>/download/<str:evaluation_id>", download, name="download-eval"),
    path("<str:org>/model", model, name="model"),
    path("<str:org>/model/<str:evaluation_id>", model, name="model-eval"),

    path("new/organization", new_organization, name="new_organization"),
    path("new/project", new_project, name="new_project"),
    path("new/server", new_server, name="new_server"),
    path("new/cache", new_cache, name="new_cache"),

    path("", home, name="home"),

    path("settings/server", edit_server, name="settingsServer"),
    path("settings/project", edit_project, name="settingsProject"),
    path("settings/organization", edit_organization, name="settingsOrganization"),
    path("settings/profile", settingsProfile, name="settingsProfile"),
]
