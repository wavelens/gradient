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

    response = None
    try:
        data = None if isinstance(body, type(None)) else json.dumps(body)
        if request_type == "GET":
            response = requests.get(url, data=data, headers=headers)
        elif request_type == "POST":
            response = requests.post(url, data=data, headers=headers)
        elif request_type == "PUT":
            response = requests.put(url, data=data, headers=headers)
        elif request_type == "PATCH":
            response = requests.patch(url, data=data, headers=headers)
        elif request_type == "DELETE":
            response = requests.delete(url, data=data, headers=headers)
    except:
        return None

    if settings.DEBUG:
        print(f'request to {url} resulted in {response}')

    if response is None:
        return None

    if response.status_code == 200:
        return response.json()
    elif response.status_code in [400, 401, 404, 409, 500]:
        try:
            return response.json()
        except ValueError:
            return {"error": True, "message": response.text}
    else:
        # Log unexpected status codes for debugging
        if settings.DEBUG:
            print(f'Unexpected status code {response.status_code} for {url}: {response.text}')
        return {"error": True, "message": f"Server returned status {response.status_code}: {response.text}"}

def health(request):
    return get_client(request.user, "health", "GET")


def post_auth_basic_register(username, name, email, password):
    return get_client(None, "auth/basic/register", "POST", body={'username': username, 'name': name, 'email': email, 'password': password})

def post_auth_check_username(username):
    return get_client(None, "auth/check-username", "POST", body={'username': username})

def post_auth_basic_login(loginname, password):
    return get_client(None, "auth/basic/login", "POST", body={'loginname': loginname, 'password': password})

def get_auth_verify_email(token):
    return get_client(None, f"auth/verify-email?token={token}", "GET")

def post_auth_resend_verification(username):
    return get_client(None, "auth/resend-verification", "POST", body={'username': username})

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

def patch_orgs_organization(request, organization=None, name=None, display_name=None, description=None):
    body = {}

    if name is not None:
        body['name'] = name
    if display_name is not None:
        body['display_name'] = display_name
    if description is not None:
        body['description'] = description

    return get_client(request.user, f"orgs/{organization}", "PATCH", body=body)

def delete_orgs_organization(request, organization):
    return get_client(request.user, f"orgs/{organization}", "DELETE")

def get_orgs_organization_users(request, organization):
    return get_client(request.user, f"orgs/{organization}/users", "GET")

def post_orgs_organization_users(request, organization, user, role):
    return get_client(request.user, f"orgs/{organization}/users", "POST", body={'user': user, 'role': role})

def patch_orgs_organization_users(request, organization, user, role):
    return get_client(request.user, f"orgs/{organization}/users", "PATCH", body={'user': user, 'role': role})

def delete_orgs_organization_users(request, organization, user):
    return get_client(request.user, f"orgs/{organization}/users", "DELETE", body={'user': user})

def get_orgs_organization_ssh(request, organization):
    return get_client(request.user, f"orgs/{organization}/ssh", "GET")

def post_orgs_organization_ssh(request, organization):
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

def patch_projects_project(request, organization, project, name=None, display_name=None, description=None, repository=None, evaluation_wildcard=None):
    return get_client(request.user, f"projects/{organization}/{project}", "PATCH", body={'name': name, 'display_name': display_name, 'description': description, 'repository': repository, 'evaluation_wildcard': evaluation_wildcard})

def delete_projects_project(request, organization, project):
    return get_client(request.user, f"projects/{organization}/{project}", "DELETE")

def post_projects_project_active(request, organization, project):
    return get_client(request.user, f"projects/{organization}/{project}/active", "POST")

def delete_projects_project_active(request, organization, project):
    return get_client(request.user, f"projects/{organization}/{project}/active", "DELETE")

def post_projects_project_check_repository(request, organization, project):
    return get_client(request.user, f"projects/{organization}/{project}/check-repository", "POST")

def post_projects_project_evaluate(request, organization, project):
    return get_client(request.user, f"projects/{organization}/{project}/evaluate", "POST")


def get_evals_evaluation(request, evaluation):
    return get_client(request.user, f"evals/{evaluation}", "GET")

def post_evals_evaluation(request, evaluation):
    return get_client(request.user, f"evals/{evaluation}", "POST")

def post_evals_evaluation_abort(request, evaluation):
    return get_client(request.user, f"evals/{evaluation}", "POST", body={'method': 'abort'})

def get_evals_evaluation_builds(request, evaluation):
    return get_client(request.user, f"evals/{evaluation}/builds", "GET")

def post_evals_evaluation_builds(request, evaluation):
    return get_client(request.user, f"evals/{evaluation}/builds", "POST")


def get_builds_build(request, build):
    return get_client(request.user, f"builds/{build}", "GET")

def post_builds_build(request, build):
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

def get_user_settings(request):
    return get_client(request.user, "user/settings", "GET")

def patch_user_settings(request, username=None, name=None, email=None):
    return get_client(request.user, "user/settings", "PATCH", body={'username': username, 'name': name, 'email': email})


def get_servers(request, organization):
    return get_client(request.user, f"servers/{organization}", "GET")

def put_servers(request, organization, name, display_name, host, port, username, architectures, features):
    return get_client(request.user, f"servers/{organization}", "PUT", body={'name': name, 'display_name': display_name, 'host': host, 'port': port, 'username': username, 'architectures': architectures, 'features': features})

def get_servers_server(request, organization, server):
    return get_client(request.user, f"servers/{organization}/{server}", "GET")

def patch_servers_server(request, organization, server, name=None, display_name=None, host=None, port=None, username=None, architectures=None, features=None):
    return get_client(request.user, f"servers/{organization}/{server}", "PATCH", body={'name': name, 'display_name': display_name, 'host': host, 'port': port, 'username': username, 'architectures': architectures, 'features': features})

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

def patch_caches_cache(request, cache, name=None, display_name=None, description=None, priority=None):
    return get_client(request.user, f"caches/{cache}", "PATCH", body={'name': name, 'display_name': display_name, 'description': description, 'priority': priority})

def delete_caches_cache(request, cache):
    return get_client(request.user, f"caches/{cache}", "DELETE")

def post_caches_cache_active(request, cache):
    return get_client(request.user, f"caches/{cache}/active", "POST")

def delete_caches_cache_active(request, cache):
    return get_client(request.user, f"caches/{cache}/active", "DELETE")


def get_commits_commit(request, commit):
    return get_client(request.user, f"commits/{commit}", "GET")

