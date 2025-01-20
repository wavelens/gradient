# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.conf import settings
import requests
import json

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

def register(request, username, name, email, password):
    return get_client(request.user, "user/register", body={'username': username, 'name': name, 'email': email, 'password': password})

def login(loginname, password):
    return get_client(None, "user/login", body={'loginname': loginname, 'password': password})

def post_organization(request, name, description, use_nix_store):
    return get_client(request.user, "organization", body={'name': name, 'description': description, 'use_nix_store': use_nix_store})

def get_user(request, uid):
    return get_client(request.user, f"user/settings/{uid}", post=False)