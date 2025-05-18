# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.shortcuts import render, redirect
from django.http import HttpResponse, HttpResponseRedirect
from django.contrib.auth.decorators import login_required
from django.contrib.auth.views import LoginView
from django.contrib.auth import logout
from . import api
from .auth import LoginForm, login, RegisterForm
from .forms import *
from django.conf import settings

@login_required
def home(request):
    details_blocks = []
    all_orgs = api.get_orgs(request)

    if isinstance(all_orgs, type(None)) or all_orgs['error']:
        return HttpResponse(status=500)

    all_orgs = all_orgs['message']

    for org in all_orgs:
        org_details = api.get_orgs_organization(request, org['name'])

        if isinstance(org_details, type(None)) or org_details['error']:
            return HttpResponse(status=500)

        org_details = org_details['message']

        details_blocks.append({
            'name': org['name'],
            'display_name': org_details['display_name'],
            'id': org['id'],
            'description': org_details['description'],
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
        })

    context = {
        'org': "TEMP",
        'details_blocks': details_blocks
    }
    return render(request, "dashboard/home.html", context)

@login_required
def workflow(request, org):
    details_blocks = []

    all_projects = api.get_projects(request, org)

    if isinstance(all_projects, type(None)) or all_projects['error']:
        return HttpResponse(status=500)

    all_projects = all_projects['message']

    for project in all_projects:
        project_details = api.get_projects_project(request, org, project['name'])

        if isinstance(project_details, type(None)) or project_details['error']:
            return HttpResponse(status=500)

        project_details = project_details['message']
        print(project_details)
        details_blocks.append({
            'project': project['name'],
            'display_name': project_details['display_name'],
            'id': project_details['last_evaluation'],
            'id2': project_details['id'],
            'description': project_details['description'],
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
        })

    context = {
        'org_id': org,
        'details_blocks': details_blocks
    }

    return render(request, "dashboard/overview.html", context)

@login_required
def log(request, org, evaluation_id=None):
    evaluation = api.get_evals_evaluation(request, evaluation_id)
    if isinstance(evaluation, type(None)) or evaluation['error']:
        return HttpResponse(status=404)
    evaluation = evaluation['message']

    project = api.get_projects(request, org)
    if isinstance(project, type(None)) or project['error']:
        return HttpResponse(status=500)
    project = [p for p in project['message'] if p['id'] == evaluation['project']]
    if len(project) == 0:
        return HttpResponse(status=404)
    project = project[0]

    commit = api.get_commits_commit(request, evaluation['commit'])
    if isinstance(commit, type(None)) or commit['error']:
        return HttpResponse(status=500)
    commit = commit['message']

    builds = api.get_evals_evaluation_builds(request, evaluation_id)
    if isinstance(builds, type(None)) or builds['error']:
        return HttpResponse(status=500)
    builds = builds['message']

    success = "waiting"
    if evaluation['status'] == 'Completed':
        success = "true"
    elif evaluation['status'] == 'Failed' or evaluation['status'] == 'Aborted':
        success = "false"

    details_blocks = [{
        'summary': "Loading Log...",
        'details': [ "Loading Log..." ]
    }]

    if success == "true":
        details_blocks = []
        for build in builds:
            build_details = api.get_builds_build(request, build['id'])

            if isinstance(build_details, type(None)) or build_details['error']:
                return HttpResponse(status=500)

            build_details = build_details['message']
            log = build_details['log'].splitlines()

            if len(log) > 1 or (len(log) > 0 and log[0] != ""):
                details_blocks.append({
                    'summary': build['name'],
                    'details': log
                })

        if len(details_blocks) == 0:
            details_blocks.append({
                'summary': "No Log available",
                'details': [ "No Log available" ]
            })

    context = {
        'org_id': org,
        'project_id': project['name'],
        'evaluation_id': evaluation_id,
        'details_blocks': details_blocks,
        'built_version' : 'Build (x86_64-linux)',
        'status' : evaluation['status'],
        'time' : '0',
        'duration' : '1s',
        'id' : '0',
        'built_name' : 'Evaluation',
        'triggerArt' : 'schedule',
        'triggerTime' : '0 months',
        'commit' : ''.join(hex(x)[2:] for x in commit['hash'][:4])[:-1],
        'branch' : 'main',
        'builds' : len(builds),
        'success' : success,
        'api_url' : settings.GRADIENT_BASE_URL,
        # 'icon' : 'green-filter'
    }

    return render(request, "dashboard/log.html", context)

@login_required
def download(request, org, evaluation_id=None):
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
        'org_id': org,
        'evaluation_id': evaluation_id,
        'files': files,
    }
    return render(request, "dashboard/download.html", context)

@login_required
def model(request, org, evaluation_id=None):
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

@login_required
def new_organization(request):
    if request.method == 'POST':
        form = NewOrganizationForm(request.POST)
        if form.is_valid():
            api.put_orgs(request, form.cleaned_data['name'], form.cleaned_data['display_name'], form.cleaned_data['description'])
            return redirect('/')
    else:
        form = NewOrganizationForm()

    return render(request, "dashboard/newOrganization.html", {'form': form})

@login_required
def new_project(request):
    org = request.GET.get("org")
    all_orgs = api.get_orgs(request)

    if isinstance(all_orgs, type(None)) or all_orgs['error']:
        return HttpResponse(status=500)

    all_orgs = all_orgs['message']

    org_choices = [ (o['name'], o['name']) for o in all_orgs ]

    if request.method == 'POST':
        form = NewProjectForm(request.POST)
        form.fields['organization'].choices = org_choices
        if form.is_valid():
            # TODO: ADD display_name
            res = api.put_projects(request, **form.cleaned_data)
            if res is None:
                form.add_error(None, "Das Projekt konnte nicht erstellt werden.")
            elif 'error' in res and res['error'] != False:
                error_msg = res['error']
                form.add_error(None, f"Projekt konnte nicht erstellt werden: {error_msg}")
                pass
            else:
                return redirect('/')
    else:
        form = NewProjectForm()
        form.fields['organization'].choices = org_choices
    return render(request, "dashboard/newProject.html", {'form': form})

@login_required
def new_server(request):
    org = request.GET.get("org")
    form = NewServerForm()
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
        return HttpResponseRedirect(self.get_success_url())
        return self.render_to_response(self.get_context_data(form=form))

    def get(self, request, *args, **kwargs):
        if request.user.is_authenticated:
            return redirect("/")
        return super().get(request, *args, **kwargs)

def logout_view(request):
    # TODO: api logout request
    logout(request)
    return redirect("/account/login")

def register(request):
    if request.method == 'POST':
        form = RegisterForm(request.POST)
        if form.is_valid():
            res = api.post_auth_basic_register(**form.cleaned_data)
            if isinstance(res, type(None)) or res['error']:
                # form = RegisterForm()
                # TODO: add form error
                pass
            else:
                return redirect('login')
    else:
        form = RegisterForm()

    return render(request, "register.html", {'form': form})
