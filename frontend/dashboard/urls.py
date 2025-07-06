# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.urls import path

from .views import *

urlpatterns = [
    path("account/login", UserLoginView.as_view(), name="login"),
    path("account/register", register, name="register"),
    path("account/logout", logout_view, name="logout"),
    path("account/check-username/", check_username_availability, name="check_username"),

    path("organization/<str:org>", workflow, name="workflow"),
    path("organization/<str:org>/log", log, name="log"),
    path("organization/<str:org>/log/<str:evaluation_id>", log, name="log-eval"),
    path("organization/<str:org>/download", download, name="download"),
    path("organization/<str:org>/download/<str:evaluation_id>", download, name="download-eval"),
    path("organization/<str:org>/model", model, name="model"),
    path("organization/<str:org>/model/<str:evaluation_id>", model, name="model-eval"),

    path("new/organization", new_organization, name="new_organization"),
    path("new/project", new_project, name="new_project"),
    path("new/server", new_server, name="new_server"),
    path("new/cache", new_cache, name="new_cache"),

    path("", home, name="home"),
    path("cache", caches, name="caches"),

    path("settings/server", edit_server, name="settingsServer"),
    path("organization/<str:org>/project/<str:project>/settings", edit_project, name="settingsProject"),
    path("organization/<str:org>/project/<str:project>/delete", delete_project, name="deleteProject"),
    path("organization/<str:org>/settings", edit_organization, name="settingsOrganization"),
    path("organization/<str:org>/delete", delete_organization, name="deleteOrganization"),
    path("organization/<str:org>/members", organization_members, name="organizationMembers"),
    path("organization/<str:org>/servers", organization_servers, name="organizationServers"),
    path("cache/<str:cache>/settings", edit_cache, name="settingsCache"),
    path("cache/<str:cache>/delete", delete_cache, name="deleteCache"),
    path("settings/profile", settingsProfile, name="settingsProfile"),
    path("settings/profile/delete", delete_user, name="deleteUser"),
]
