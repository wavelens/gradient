# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.urls import path

from .views import *

urlpatterns = [
    path("account/login", UserLoginView.as_view(), name="login"),
    path("account/register", register, name="register"),
    path("account/logout", logout_view, name="logout"),

    path("<str:org_id>", workflow, name="workflow"),
    path("<str:org_id>/log", log, name="log"),
    path("<str:org_id>/log/<str:evaluation_id>", log, name="log-eval"),
    path("<str:org_id>/download", download, name="download"),
    path("<str:org_id>/download/<str:evaluation_id>", download, name="download-eval"),
    path("<str:org_id>/model", model, name="model"),
    path("<str:org_id>/model/<str:evaluation_id>", model, name="model-eval"),

    path("new/organization", new_organization, name="new_organization"),
    path("new/project", new_project, name="new_project"),
    path("new/server", new_server, name="new_server"),

    path("", home, name="home"),
]
