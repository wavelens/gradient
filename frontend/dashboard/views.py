# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.shortcuts import render, redirect
from django.http import HttpResponse, HttpResponseRedirect
from django.contrib.auth.decorators import login_required
from django.contrib.auth.views import LoginView
from django.contrib.auth import logout
from django.contrib import messages
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
def edit_organization(request, org):
    org_data = api.get_orgs_organization(request, org)
    org_message = org_data.get('message', {})
    initial_data = {
        'name': org_message.get('name', ''),
        'display_name': org_message.get('display_name', ''),
        'description': org_message.get('description', '')
    }

    if request.method == 'POST':
        form = EditOrganizationForm(request.POST)
        if form.is_valid():
            cleaned = form.cleaned_data
            patch_data = {}

            if cleaned['name'] != org_message.get('name'):
                patch_data['name'] = cleaned['name']
            if cleaned['display_name'] != org_message.get('display_name'):
                patch_data['display_name'] = cleaned['display_name']
            if cleaned['description'] != org_message.get('description'):
                patch_data['description'] = cleaned['description']
            
            if patch_data:
                response = api.patch_orgs_organization(request, org, **patch_data)
                if response.get("error"):
                    form.add_error(None, response.get("message", "Unbekannter Fehler"))
                else:
                    return redirect('/')
            else:
                return redirect('/')
    else:
        form = EditOrganizationForm(initial=initial_data)

    return render(request, "dashboard/settings/organization.html", {'form': form, 'org': org})

@login_required
def delete_organization(request, org):
    if request.method == 'POST':
        response = api.delete_orgs_organization(request, org)
        if response is None or response.get("error"):
            messages.error(request, "Failed to delete organization.")
            return redirect('settingsOrganization', org=org)
        else:
            messages.success(request, "Organization deleted successfully.")
            return redirect('home')
    else:
        return redirect('settingsOrganization', org=org)

@login_required
def new_cache(request):
    if request.method == 'POST':
        form = NewCacheForm(request.POST)
        if form.is_valid():
            api.put_caches(request, form.cleaned_data['name'], form.cleaned_data['display_name'], form.cleaned_data['description'], form.cleaned_data['priority'])
            return redirect('/')
    else:
        form = NewCacheForm()

    return render(request, "dashboard/NewCache.html", {'form': form})

@login_required
def edit_cache(request, cache):
    cache_data = api.get_caches_cache(request, cache)
    cache_message = cache_data.get('message', {})
    initial_data = {
        'name': cache_message.get('name', ''),
        'display_name': cache_message.get('display_name', ''),
        'description': cache_message.get('description', ''),
        'priority': cache_message.get('priority', '')
    }

    if request.method == 'POST':
        form = EditCacheForm(request.POST)
        if form.is_valid():
            cleaned = form.cleaned_data
            patch_data = {}

            if cleaned['name'] != cache_message.get('name'):
                patch_data['name'] = cleaned['name']
            if cleaned['display_name'] != cache_message.get('display_name'):
                patch_data['display_name'] = cleaned['display_name']
            if cleaned['description'] != cache_message.get('description'):
                patch_data['description'] = cleaned['description']
            if cleaned['priority'] != cache_message.get('priority'):
                patch_data['priority'] = cleaned['priority']
            
            if patch_data:
                response = api.patch_caches_cache(request, cache, **patch_data)
                if response.get("error"):
                    form.add_error(None, response.get("message", "Unbekannter Fehler"))
                else:
                    return redirect('/')
            else:
                return redirect('/')
    else:
        form = EditCacheForm(initial=initial_data)

    return render(request, "dashboard/settings/cache.html", {'form': form, 'cache': cache})

@login_required
def delete_cache(request, cache):
    if request.method == 'POST':
        response = api.delete_caches_cache(request, cache)
        if response is None or response.get("error"):
            messages.error(request, "Failed to delete cache.")
            return redirect('settingsCache', cache=cache)
        else:
            messages.success(request, "Cache deleted successfully.")
            return redirect('caches')
    else:
        return redirect('settingsCache', cache=cache)

@login_required
def organization_members(request, org):
    members_data = api.get_orgs_organization_users(request, org)
    if isinstance(members_data, type(None)) or members_data.get('error'):
        members = []
    else:
        members = members_data.get('message', [])
    
    add_form = None
    if request.method == 'POST':
        if 'add_member' in request.POST:
            add_form = AddOrganizationMemberForm(request.POST)
            if add_form.is_valid():

                response = api.post_orgs_organization_users(
                    request, 
                    org, 
                    add_form.cleaned_data['user'], 
                    add_form.cleaned_data['role'].upper()
                )

                if response and not response.get('error'):
                    return redirect(f'/organization/{org}/members')
                else:
                    error_message = response.get('message') if response else 'Failed to add member (no response from API)'
                    add_form.add_error(None, error_message)
        
        elif 'remove_member' in request.POST:
            user_to_remove = request.POST.get('user')
            if user_to_remove:
                response = api.delete_orgs_organization_users(request, org, user_to_remove)
                if response and not response.get('error'):
                    return redirect(f'/organization/{org}/members')
        
        elif 'edit_role' in request.POST:
            user_to_edit = request.POST.get('user')
            new_role = request.POST.get('role')
            if user_to_edit and new_role:
                response = api.patch_orgs_organization_users(request, org, user_to_edit, new_role)
                if response and not response.get('error'):
                    return redirect(f'/organization/{org}/members')
    
    if not add_form:
        add_form = AddOrganizationMemberForm()
    
    context = {
        'org': org,
        'members': members,
        'add_form': add_form,
        'role_choices': AddOrganizationMemberForm.ROLE_CHOICES
    }
    
    return render(request, "dashboard/settings/organization_members.html", context)

@login_required
def caches(request):
    print('Hello')
    details_blocks = []
    all_caches = api.get_caches(request)
    print(all_caches)
    if isinstance(all_caches, type(None)):
        # API call failed - show empty state
        context = {'details_blocks': []}
        return render(request, "dashboard/caches.html", context)
    
    if all_caches.get('error'):
        # API returned error - show empty state with error message
        context = {'details_blocks': [], 'error_message': all_caches.get('message', 'Failed to load caches')}
        return render(request, "dashboard/caches.html", context)

    all_caches = all_caches.get('message', [])

    for cache in all_caches:
        cache_details = api.get_caches_cache(request, cache['name'])

        if isinstance(cache_details, type(None)) or cache_details.get('error'):
            # Skip this cache if we can't get details
            continue

        cache_details = cache_details['message']

        details_blocks.append({
            'name': cache['name'],
            'display_name': cache_details['display_name'],
            'id': cache['id'],
            'description': cache_details['description'],
            'priority': cache_details.get('priority', 'N/A'),
            'status': cache_details.get('status', 'inactive'),
            'size': cache_details.get('size', 'N/A'),
            'hit_rate': cache_details.get('hit_rate', 'N/A'),
        })

    context = {
        'details_blocks': details_blocks
    }
    return render(request, "dashboard/caches.html", context)

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
def edit_project(request, org, project):
    project_data = api.get_projects_project(request, org, project)
    project_message = project_data.get('message', {})
    initial_data = {
        'name': project_message.get('name', ''),
        'display_name': project_message.get('display_name', ''),
        'description': project_message.get('description', ''),
        'repository': project_message.get('repository', ''),
        'evaluation_wildcard': project_message.get('evaluation_wildcard', '')
    }

    if request.method == 'POST':
        form = EditProjectForm(request.POST)
        if form.is_valid():
            cleaned = form.cleaned_data
            patch_data = {}
            if cleaned['name'] != project_message.get('name'):
                patch_data['name'] = cleaned['name']
            if cleaned['display_name'] != project_message.get('display_name'):
                patch_data['display_name'] = cleaned['display_name']
            if cleaned['description'] != project_message.get('description'):
                patch_data['description'] = cleaned['description']

            if patch_data:
                response = api.patch_projects_project(request, org, project, **patch_data)
                if response.get("error"):
                    form.add_error(None, response.get("message", "Unbekannter Fehler"))
                else:
                    return redirect('/')
            else:
                return redirect('/')
    else:
        form = EditProjectForm(initial=initial_data)
    return render(request, "dashboard/settings/project.html", {'form': form, 'org': org, 'project': project})

@login_required
def delete_project(request, org, project):
    if request.method == 'POST':
        response = api.delete_projects_project(request, org, project)
        if response is None or response.get("error"):
            messages.error(request, "Failed to delete project.")
            return redirect('settingsProject', org=org, project=project)
        else:
            messages.success(request, "Project deleted successfully.")
            return redirect('workflow', org=org)
    else:
        return redirect('settingsProject', org=org, project=project)

@login_required
def new_server(request):
    org = request.GET.get("org")
    form = NewServerForm()
    return render(request, "dashboard/newServer.html", {'form': form})

@login_required
def edit_server(request):
    org = request.GET.get("org")
    form = EditServerForm()
    return render(request, "dashboard/settings/server.html", {'form': form})

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

def settingsProfile(request):
    user = request.user
    initial_data = {
        'name': user.name,
        'username': user.username,
        'email': user.email
    }
    if request.method == 'POST':
        form = EditUserForm(request.POST)
        if form.is_valid():
            cleaned = form.cleaned_data
            patch_data = {}
            if cleaned['name'] != user.name:
                patch_data['name'] = cleaned['name']
            if cleaned['username'] != user.username:
                patch_data['username'] = cleaned['username']
            if cleaned['email'] != user.email:
                patch_data['email'] = cleaned['email']

            if patch_data:
                response = api.patch_user_settings(request, **patch_data)
                if response.get("error"):
                    form.add_error(None, response.get("message", "Unbekannter Fehler"))
                else:
                    return redirect('/')
            else:
                return redirect('/')
    else:
        form = EditUserForm(initial=initial_data)
    return render(request, "dashboard/settings/profile.html", {'form': form})

@login_required
def delete_user(request):
    if request.method == 'POST':
        response = api.delete_user(request)
        if response is None or response.get("error"):
            messages.error(request, "Failed to delete account.")
            return redirect('settingsProfile')
        else:
            logout(request)
            messages.success(request, "Account deleted successfully.")
            return redirect('login')
    else:
        return redirect('settingsProfile')