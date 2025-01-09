# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.conf import settings
import requests
import json

def get_client(user, endpoint, body=None, post=True, login=True):
    headers = {'Content-Type': 'application/json'}

    if login:
        headers['Authorization'] = 'Bearer ' + user.session}
    else:
        return None

    url = f"{settings.GRADIENT_BASE_URL}/{endpoint}"

    if post:
        response = requests.post(url, data=json.dumps(body), headers=headers)
        if response.status_code == 200:
            return response.json()
        else:
            return None
    else:
        response = requests.get(url, headers=headers)
        if response.status_code == 200:
            return response.json()
        else:
            return None

def health(request):
    return get_client(request.user, "health", post=False, login=False)

def register(request, username, name, email, password):
    return get_client(request.user, "register", body={'username': username, 'name': name, 'email': email, 'password': password})

def login(user, loginname, password):
    return get_client(user, "login", body={'loginname': loginname, 'password': password})
