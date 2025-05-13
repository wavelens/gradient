# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django import forms
from django.utils.translation import gettext_lazy as _
from django.contrib.auth.models import AnonymousUser
from django.contrib.auth.signals import user_logged_in, user_logged_out, user_login_failed
from django.contrib.auth import SESSION_KEY, BACKEND_SESSION_KEY, _clean_credentials
from django.utils.deprecation import MiddlewareMixin
from django.utils.functional import SimpleLazyObject
from django.db.models.manager import EmptyManager
from django.contrib.auth.models import Group, Permission
from functools import partial
from . import api
from django.templatetags.static import static


def login(request, user):
    request.user = user
    request.session[SESSION_KEY] = user.session


def logout(request):
    user = getattr(request, 'user', None)
    user_logged_out.send(sender=user.__class__, request=request, user=user)

    request.session.flush()

    if hasattr(request, 'user'):
        request.user = AnonymousUser()


def get_user(request):
    user = None

    if SESSION_KEY in request.session:
        json_user_cache = api.get_user(request.session[SESSION_KEY])

        if json_user_cache is None or json_user_cache['error']:
            request.session.pop(SESSION_KEY, None)
            return AnonymousUser()
        else:
            json_user_cache = json_user_cache['message']

        json_user_cache['session'] = request.session[SESSION_KEY]
        user = User(json_user_cache)

    return user or AnonymousUser()

def get_cached_user(request):
    if not hasattr(request, "_cached_user"):
        request._cached_user = get_user(request)
    return request._cached_user

class AuthenticationMiddleware(MiddlewareMixin):
    def process_request(self, request):
        if not hasattr(request, "session"):
            raise ImproperlyConfigured(
                "The Django authentication middleware requires session "
                "middleware to be installed. Edit your MIDDLEWARE setting to "
                "insert "
                "'django.contrib.sessions.middleware.SessionMiddleware' before "
                "'django.contrib.auth.middleware.AuthenticationMiddleware'."
            )

        request.user = SimpleLazyObject(lambda: get_cached_user(request))
        # request.auser = partial(auser, request)


class LoginForm(forms.Form):
    username = forms.CharField(
        label = _("E-Mail or Username"),
        max_length = 150,
        widget = forms.TextInput(attrs={'class': 'form-control'}),
        required = True
    )
    password = forms.CharField(
        label = _("Password"),
        widget = forms.PasswordInput(attrs={'class': 'form-control'}),
        required = True
    )
    remember_me = forms.BooleanField(
        label = _("Stay logged in"),
        required = False,
        widget = forms.CheckboxInput(attrs={'class': 'form-check-input'})
    )

    error_messages = {
        'invalid_login': _("Please enter a correct %(username)s and password. "
                           "Note that both fields may be case-sensitive."),
        'inactive': _("This account is inactive."),
    }

    def __init__(self, request=None, *args, **kwargs):
        self.request = request
        self.user_cache = None
        super().__init__(*args, **kwargs)

    def clean(self):
        username = self.cleaned_data.get('username')
        password = self.cleaned_data.get('password')

        if username is not None and password:
            user_session = api.post_auth_basic_login(username, password)
            if user_session is None or user_session['error']:
                # TODO: fix reporting
                # user_login_failed.send(sender=__name__, credentials=_clean_credentials(username, password))
                raise forms.ValidationError(
                    self.error_messages['invalid_login'],
                    code='invalid_login',
                    params={'username': username},
                )
            else:
                # self.confirm_login_allowed(self.user_cache)
                user_session = user_session['message']

            json_user_cache = api.get_user(user_session)

            if json_user_cache is None or json_user_cache['error']:
                raise forms.ValidationError(
                    self.error_messages['invalid_login'],
                    code='invalid_login',
                    params={'username': username},
                )
            else:
                json_user_cache = json_user_cache['message']

            json_user_cache['session'] = user_session
            self.user_cache = User(json_user_cache)

        return self.cleaned_data

    def get_user(self):
        return self.user_cache

class RegisterForm(forms.Form):
    username = forms.CharField(
        label = _("Username"),
        max_length = 150,
        widget = forms.TextInput(attrs={'class': 'form-control'}),
        required = True
    )

    name = forms.CharField(
        label = _("Name"),
        max_length = 150,
        widget = forms.TextInput(attrs={'class': 'form-control'}),
        required = True
    )

    email = forms.CharField(
        label = _("E-Mail"),
        max_length = 150,
        widget = forms.TextInput(attrs={'class': 'form-control'}),
        required = True
    )

    password = forms.CharField(
        label = _("Password"),
        widget = forms.PasswordInput(attrs={'class': 'form-control'}),
        required = True
    )

class User(object):
    id = None
    pk = None
    username = ''
    is_staff = False
    is_active = False
    is_superuser = False
    is_authenticated = True
    _groups = EmptyManager(Group)
    _user_permissions = EmptyManager(Permission)

    def __init__(self, json=None, session=None):
        if json:
            self.id = json['id']
            self.username = json['username']
            self.email = json['email']
            self.name = json['name']
            self.session = json['session']

        if session:
            self.session = session

        self.image = static('dashboard/images/pb.png')

    def __str__(self):
        return self.name

    def __eq__(self, other):
        return isinstance(other, self.__class__)

    def __ne__(self, other):
        return not self.__eq__(other)

    def __hash__(self):
        return self.id

    def save(self):
        raise NotImplementedError("Django doesn't provide a DB representation for User.")

    def delete(self):
        raise NotImplementedError("Django doesn't provide a DB representation for User.")

    def set_password(self, raw_password):
        raise NotImplementedError("Django doesn't provide a DB representation for User.")

    def check_password(self, raw_password):
        raise NotImplementedError("Django doesn't provide a DB representation for User.")

    def _get_groups(self):
        return self._groups
    groups = property(_get_groups)

    def _get_user_permissions(self):
        return self._user_permissions
    user_permissions = property(_get_user_permissions)

    def get_group_permissions(self, obj=None):
        return set()

    def get_all_permissions(self, obj=None):
        return _user_get_all_permissions(self, obj=obj)

    def has_perm(self, perm, obj=None):
        return _user_has_perm(self, perm, obj=obj)

    def has_perms(self, perm_list, obj=None):
        for perm in perm_list:
            if not self.has_perm(perm, obj):
                return False
        return True

    def has_module_perms(self, module):
        return _user_has_module_perms(self, module)

    def is_anonymous(self):
        return False

    def is_authenticated(self):
        return True

    def get_username(self):
        return self.username
