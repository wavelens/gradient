# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0

from django.urls import path

from . import views

urlpatterns = [
    path("workflow", views.workflow, name="workflow"),
    path("log", views.log, name="log"),
    path("download", views.download, name="download"),
    path("model", views.model, name="model"),
]
