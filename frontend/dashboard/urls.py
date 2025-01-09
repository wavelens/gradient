# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.urls import path

from views import *

urlpatterns = [
    path("account/login/", UserLoginView.as_view(), name="login"),
    path("account/logout/", logout_view, name="logout"),

    path("workflow/<str:org_id>", workflow, name="workflow"),
    path("log", log, name="log"),
    path("download", download, name="download"),
    path("model", model, name="model"),
    path("newOrganization", newOrganization, name="newOrganization"),
    path("newProject", newProject, name="newProject"),
    path("newServer", newServer, name="newServer"),
]
