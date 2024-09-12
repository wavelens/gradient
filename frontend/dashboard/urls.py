from django.urls import path

from . import views

urlpatterns = [
    path("workflow", views.workflow, name="workflow"),
    path("log", views.log, name="log"),
    path("download", views.download, name="download"),
]
