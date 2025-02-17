# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.urls import path

from .views import *

urlpatterns = [
    path("account/login/", UserLoginView.as_view(), name="login"),
    path("account/logout/", logout_view, name="logout"),

    path("<str:org_id>/", workflow, name="workflow"),
    path("<str:org_id>/log/", log, name="log"),
    path("<str:org_id>/download/", download, name="download"),
    path("<str:org_id>/model/", model, name="model"),
    
    path("new/organization/", new_organization, name="new_organization"),
    path("<str:org_id>/new/project/", new_project, name="new_project"),
    path("<str:org_id>/new/server/", new_server, name="new_server"),
]
