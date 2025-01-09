# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.shortcuts import render
from django.http import HttpResponse
from django.template import loader
from django.shortcuts import render
from django.contrib.auth.decorators import login_required
from .auth import LoginForm, login
from .forms import *
import api

@login_required(login_url='/account/login/')
def workflow(request, org_id):
    details_blocks = [
        {
            'project': "build",
            'id': "wvls:build",
            'exec': 34,
            'duration': '12m 11s',
            'performance': 'filter',
            'latest_runs': 'filter',
            'latestRuns': {
                '1': 'true',
                '2': 'true',
                '3': 'false',
                '4': 'true',
                '5': 'true',
            },
            'wfp': {
                '1': 'true',
                '2': 'false',
                '3': 'nothing',
            }
        },
        {
            'project': "build2",
            'id': "wvls:build",
            'exec': 34,
            'duration': '12m 11s',
            'performance': 'filter',
            'latest_runs': 'filter',
            'latestRuns': {
                '1': 'false',
                '2': 'true',
                '3': 'false',
                '4': 'true',
                '5': 'true',
            },
            'wfp': {
                '1': 'true',
                '2': 'false',
                '3': 'nothing',
            }
        },
        {
            'project': "build3",
            'id': "wvls:build",
            'exec': 34,
            'duration': '12m 11s',
            'performance': 'filter',
            'latest_runs': 'filter',
            'latestRuns': {
                '1': 'false',
                '2': 'false',
                '3': 'false',
                '4': 'true',
                '5': 'true',
            },
            'wfp': {
                '1': 'true',
                '2': 'false',
                '3': 'nothing',
            }
        },
        {
            'project': "build4",
            'id': "wvls:build",
            'exec': 34,
            'duration': '12m 11s',
            'performance': 'filter',
            'latest_runs': 'filter',
            'latestRuns': {
                '1': 'nothing',
                '2': 'nothing',
                '3': 'false',
                '4': 'true',
                '5': 'true',
            },
            'wfp': {
                '1': 'true',
                '2': 'false',
                '3': 'nothing',
            }
        },
    ]
    context = {
        'details_blocks': details_blocks
    }
    return render(request, "dashboard/overview.html", context)

def log(request):
    details_blocks = [
        {
            'summary': "Lorem ipsum dolor sit amet, consetetur sadipscing elitr",
            'details': [
                "Lorem ipsum dolor sit amet, consetetur sadipscing elitr",
                "Sed diam nonumy eirmod tempor invidunt ut labore et dolore magna",
                "Aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores",
                "Et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet.",
                "Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua.",
                "At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet."
            ]
        },
        {
            'summary': "Summary 2",
            'details': [
                "Detail 2-1",
                "Detail 2-2",
                "Detail 2-3"
            ]
        }
    ]
    context = {
        'details_blocks': details_blocks,
        'built_version' : 'Vbuild (x86_64-linux)',
        'status' : 'Vsucceeded',
        'time' : 'V2',
        'duration' : '1s',
        'id' : 'v940',
        'built_name' : 'Vdataset.corpus',
        'triggerArt' : 'schedule',
        'triggerTime' : '8 months',
        'git' : 'f72bjds',
        'branch' : 'main',
        'artifacts' : '-',
        # 'icon' : 'green-filter'
    }
    return render(request, "dashboard/log.html", context)

def download(request):
    files = [
    {
        'file': "File 1",
        'type': "dataset",
        'link' : "dataset.zip",
        'actions' : "Details"
    },
    {
        'file': "File 2",
        'type': "dataset",
        'link' : "dataset.zip",
        'actions' : "Details"
    },
    {
        'file': "File 3",
        'type': "dataset",
        'link' : "dataset.zip",
        'actions' : "Details"
    },
    ]
    context = {
        'files': files,
    }
    return render(request, "dashboard/download.html", context)

def model(request):
    models = [
    {
        'name': "Model 1",
        'description': "bliblablubs"
    },
    {
        'name': "Model 2",
        'description': "hihaho"
    }
    ]
    context = {
        'models': models,
    }
    return render(request, "dashboard/model.html", context)

def newOrganization(request):
    if request.method == 'POST':
        form = newOrganizationForm(request.POST)
        if form.is_valid():
            api.new_organization(request, form.cleaned_data['name'], form.cleaned_data['description'])
            return redirect('dashboard')
    else:
        form = newOrganizationForm()

    return render(request, "dashboard/newOrganization.html", {'form': form})

def newProject(request):
    form = newProjectForm()
    return render(request, "dashboard/newProject.html", {'form': form})

def newServer(request):
    form = newServerForm()
    return render(request, "dashboard/newServer.html", {'form': form})

class UserLoginView(LoginView):
    template_name = "login.html"
    form_class = LoginForm

    def form_valid(self, form):
        login(self.request, form.get_user())
        # if not form.get_user().is_active:
        #     return render(
        #         self.request,
        #         "checkin_displaytext.html",
        #         {
        #             "displaytext": _(
        #                 "Your account is not active yet! Please verify your E-Mail first!"
        #             )
        #         },
        #     )
        #     return redirect(self.get_success_url())
        # self.request.session["allauth_2fa_user_id"] = form.get_user().pk
        return redirect("home")

def logout_view(request):
    logout(request)
    return redirect("/account/login/")

