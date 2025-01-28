# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.conf import settings
import requests
import json
from .auth import User

def get_client(user, endpoint, body=None, post=True):
    headers = {'Content-Type': 'application/json'}

    if not (isinstance(user, type(None)) or isinstance(user.session, type(None))):
        headers['Authorization'] = 'Bearer ' + user.session

    url = f"{settings.GRADIENT_BASE_URL}/{endpoint}"

    if post:
        try:
            response = requests.post(url, data=json.dumps(body), headers=headers)
        except:
            return None
        print(response)
        print(url)
        if response.status_code == 200:
            return response.json()
        else:
            return None
    else:
        try:
            response = requests.get(url, headers=headers)
        except:
            return None

        if response.status_code == 200:
            return response.json()
        else:
            return None

def health(request):
    return get_client(request.user, "health", post=False)

def register(username, name, email, password):
    return get_client(None, "user/register", body={'username': username, 'name': name, 'email': email, 'password': password})

def login(loginname, password):
    return get_client(None, "user/login", body={'loginname': loginname, 'password': password})

def logout(request):
    return get_client(request.user, "user/logout", post=True)

def post_organization(request, name, description, use_nix_store):
    return get_client(request.user, "organization", body={'name': name, 'description': description, 'use_nix_store': use_nix_store})

def get_organization(request, organization_id):
    return get_client(request.user, f"organization/{organization_id}", post=False)

def get_user(request, uid):
    return get_client(request.user, f"user/settings/{uid}", post=False)

def get_user_info(session):
    user = User(session=session)
    return get_client(user, f"user/info", post=False)

def post_project(request, name, description, repository, evaluation_wildcard):
    return get_client(request.user, "project", body={'name': name, 'description': description, 'repository': repository, 'evaluation_wildcard': evaluation_wildcard})

def get_project(request, project_id):
    return get_client(request.user, f"project/{project_id}", post=False)

def post_server(request, organization_id, name, host, port, username, architectures, features):
    return get_client(request.user, "server", body={ 'organization_id': organization_id, 'name': name, 'host': host, 'port': port, 'username': username, 'architectures': architectures, 'features': features})

def get_servers(request):
    return get_client(request.user, "server", post=False)

def post_build(request, log_streaming):
    return get_client(request.user, "build", body={'log_streaming': log_streaming})

def get_build(request, build_id):
    return get_client(request.user, f"build/{build_id}", post=False)

def post_api_key(request, name):
    return get_client(request.user, "user/api", body={'name': name})

def post_project_check_repository(request, project_id):
    return get_client(request.user, f"project/{project_id}/check-repository", post=True)

def post_organization_ssh(request, organization_id):
    return get_client(request.user, f"organization/{organization_id}/ssh", post=True)

def post_server_check(request, server_id):
    return get_client(request.user, f"server/{server_id}/check", post=True)

