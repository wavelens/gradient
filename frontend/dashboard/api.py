# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.conf import settings
import requests
import json

class FakeUser:
    def __init__(self, session):
        self.session = session

def get_client(user, endpoint, request_type, body=None):
    headers = {'Content-Type': 'application/json'}

    if not (isinstance(user, type(None)) or isinstance(user.session, type(None))):
        headers['Authorization'] = 'Bearer ' + user.session

    url = f"{settings.GRADIENT_BASE_URL}/{endpoint}"

    try:
        data = None if isinstance(body, type(None)) else json.dumps(body)
        if request_type == "GET":
            response = requests.get(url, data=data, headers=headers)
        elif request_type == "POST":
            response = requests.post(url, data=data, headers=headers)
        elif request_type == "PUT":
            response = requests.put(url, data=data, headers=headers)
        elif request_type == "DELETE":
            response = requests.delete(url, data=data, headers=headers)
    except:
        return None

    if settings.DEBUG:
        print(f'request to {url} resulted in {response}')

    if response.status_code == 200:
        return response.json()
    else:
        return None

def health(request):
    return get_client(request.user, "health", "GET")


def post_auth_basic_register(username, name, email, password):
    return get_client(None, "auth/basic/register", "POST", body={'username': username, 'name': name, 'email': email, 'password': password})

def post_auth_basic_login(loginname, password):
    return get_client(None, "auth/basic/login", "POST", body={'loginname': loginname, 'password': password})

def post_auth_oauth_authorize(code):
    return get_client(None, f"auth/oauth/authorize?code={code}", "POST")

def get_auth_oauth_authorize():
    return get_client(None, "auth/oauth/authorize", "GET")

def post_auth_logout(request):
    return get_client(request.user, "auth/logout", "POST")


def get_orgs(request):
    return get_client(request.user, "orgs", "GET")

def put_orgs(request, name, display_name, description):
    return get_client(request.user, "orgs", "PUT", body={'name': name, 'display_name': display_name, 'description': description})

def get_orgs_organization(request, organization):
    return get_client(request.user, f"orgs/{organization}", "GET")

def delete_orgs_organization(request, organization):
    return get_client(request.user, f"orgs/{organization}", "DELETE")

def get_orgs_organization_ssh(request, organization):
    return get_client(request.user, f"orgs/{organization}/ssh", "GET")

def post_orgs_organization_ssh(request, organization, public_key):
    return get_client(request.user, f"orgs/{organization}/ssh", "POST")

def post_orgs_organization_subscribe_cache(request, organization, cache):
    return get_client(request.user, f"orgs/{organization}/subscribe-cache/{cache}", "POST")

def delete_orgs_organization_subscribe_cache(request, organization, cache):
    return get_client(request.user, f"orgs/{organization}/subscribe-cache/{cache}", "DELETE")


def get_projects(request, organization):
    return get_client(request.user, f"projects/{organization}", "GET")

def put_projects(request, organization, name, display_name, description, repository, evaluation_wildcard):
    return get_client(request.user, f"projects/{organization}", "PUT", body={'name': name, 'display_name': display_name, 'description': description, 'repository': repository, 'evaluation_wildcard': evaluation_wildcard})

def get_projects_project(request, organization, project):
    return get_client(request.user, f"projects/{organization}/{project}", "GET")

def delete_projects_project(request, organization, project):
    return get_client(request.user, f"projects/{organization}/{project}", "DELETE")

def post_projects_project_active(request, organization, project):
    return get_client(request.user, f"projects/{organization}/{project}/active", "POST")

def delete_projects_project_active(request, organization, project):
    return get_client(request.user, f"projects/{organization}/{project}/active", "DELETE")

def post_projects_project_check_repository(request, organization, project):
    return get_client(request.user, f"projects/{organization}/{project}/check-repository", "POST")

def post_projects_project_evaluations(request, organization, project):
    return get_client(request.user, f"projects/{organization}/{project}/evaluations", "POST")


def get_evals_evaluation(request, evaluation):
    return get_client(request.user, f"evaluations/{evaluation}", "GET")

def post_evals_evaluation(request, evaluation):
    return get_client(request.user, f"evaluations/{evaluation}", "POST")

def get_evals_evaluation_builds(request, evaluation):
    return get_client(request.user, f"evaluations/{evaluation}/builds", "GET")

def connect_evals_evaluation_builds(request, evaluation):
    return get_client(request.user, f"evaluations/{evaluation}/builds", "POST")


def get_builds_build(request, build):
    return get_client(request.user, f"builds/{build}", "GET")

def connect_builds_build(request, build):
    return get_client(request.user, f"builds/{build}", "POST")


def get_user(session):
    user = FakeUser(session)
    return get_client(user, "user", "GET")

def delete_user(request):
    return get_client(request.user, "user", "DELETE")

def post_user_keys(request, name):
    return get_client(request.user, "user/keys", "POST", body={'name': name})

def delete_user_keys(request, name):
    return get_client(request.user, f"user/keys/{name}", "DELETE")


def get_servers(request, organization):
    return get_client(request.user, f"servers/{organization}", "GET")

def put_servers(request, organization, name, display_name, host, port, username, architectures, features):
    return get_client(request.user, f"servers/{organization}", "PUT", body={'name': name, 'display_name': display_name, 'host': host, 'port': port, 'username': username, 'architectures': architectures, 'features': features})

def get_servers_server(request, organization, server):
    return get_client(request.user, f"servers/{organization}/{server}", "GET")

def delete_servers_server(request, organization, server):
    return get_client(request.user, f"servers/{organization}/{server}", "DELETE")

def post_servers_server_active(request, organization, server):
    return get_client(request.user, f"servers/{organization}/{server}/active", "POST")

def delete_servers_server_active(request, organization, server):
    return get_client(request.user, f"servers/{organization}/{server}/active", "DELETE")

def post_servers_server_check_connection(request, organization, server):
    return get_client(request.user, f"servers/{organization}/{server}/check-connection", "POST")


def get_caches(request):
    return get_client(request.user, "caches", "GET")

def put_caches(request, name, display_name, description, priority):
    return get_client(request.user, "caches", "PUT", body={'name': name, 'display_name': display_name, 'description': description, 'priority': priority})

def get_caches_cache(request, cache):
    return get_client(request.user, f"caches/{cache}", "GET")

def delete_caches_cache(request, cache):
    return get_client(request.user, f"caches/{cache}", "DELETE")

def post_caches_cache_active(request, cache):
    return get_client(request.user, f"caches/{cache}/active", "POST")

def delete_caches_cache_active(request, cache):
    return get_client(request.user, f"caches/{cache}/active", "DELETE")


def get_commits_commit(request, commit):
    return get_client(request.user, f"commits/{commit}", "GET")

