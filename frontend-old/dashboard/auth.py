# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django import forms
from django.utils.translation import gettext_lazy as _
from django.contrib.auth.models import AnonymousUser
from django.contrib.auth.signals import (
    user_logged_in,
    user_logged_out,
    user_login_failed,
)
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
    user = getattr(request, "user", None)
    user_logged_out.send(sender=user.__class__, request=request, user=user)

    request.session.flush()

    if hasattr(request, "user"):
        request.user = AnonymousUser()


def get_user(request):
    user = None

    if SESSION_KEY in request.session:
        json_user_cache = api.get_user(request.session[SESSION_KEY])

        if json_user_cache is None or json_user_cache["error"]:
            request.session.pop(SESSION_KEY, None)
            return AnonymousUser()
        else:
            json_user_cache = json_user_cache["message"]

        json_user_cache["session"] = request.session[SESSION_KEY]
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
        label=_("E-Mail or Username"),
        max_length=150,
        widget=forms.TextInput(attrs={"class": "form-control"}),
        required=True,
    )
    password = forms.CharField(
        label=_("Password"),
        widget=forms.PasswordInput(attrs={"class": "form-control"}),
        required=True,
    )
    remember_me = forms.BooleanField(
        label=_("Stay logged in"),
        required=False,
        widget=forms.CheckboxInput(attrs={"class": "form-check-input"}),
    )

    error_messages = {
        "invalid_login": _(
            "Invalid username or password. Please check your credentials and try again."
        ),
        "inactive": _(
            "This account is inactive. Please contact support for assistance."
        ),
        "network_error": _(
            "Unable to connect to the server. Please check your internet connection and try again."
        ),
        "server_error": _(
            "The server is temporarily unavailable. Please try again later."
        ),
    }

    def __init__(self, request=None, *args, **kwargs):
        self.request = request
        self.user_cache = None
        super().__init__(*args, **kwargs)

    def clean(self):
        username = self.cleaned_data.get("username")
        password = self.cleaned_data.get("password")
        remember_me = self.cleaned_data.get("remember_me", False)

        if username is not None and password:
            user_session = api.post_auth_basic_login(username, password, remember_me)
            if user_session is None:
                raise forms.ValidationError(
                    self.error_messages["network_error"],
                    code="network_error",
                )
            elif user_session.get("error"):
                error_message = user_session.get("message", "")
                if (
                    "invalid" in error_message.lower()
                    or "incorrect" in error_message.lower()
                ):
                    raise forms.ValidationError(
                        self.error_messages["invalid_login"],
                        code="invalid_login",
                        params={"username": username},
                    )
                elif (
                    "inactive" in error_message.lower()
                    or "disabled" in error_message.lower()
                ):
                    raise forms.ValidationError(
                        self.error_messages["inactive"],
                        code="inactive",
                    )
                else:
                    raise forms.ValidationError(
                        error_message or self.error_messages["server_error"],
                        code="server_error",
                    )
            else:
                user_session = user_session["message"]

            json_user_cache = api.get_user(user_session)

            if json_user_cache is None:
                raise forms.ValidationError(
                    self.error_messages["network_error"],
                    code="network_error",
                )
            elif json_user_cache.get("error"):
                error_message = json_user_cache.get("message", "")
                raise forms.ValidationError(
                    error_message or self.error_messages["server_error"],
                    code="server_error",
                )
            else:
                json_user_cache = json_user_cache["message"]

            json_user_cache["session"] = user_session
            self.user_cache = User(json_user_cache)

        return self.cleaned_data

    def get_user(self):
        return self.user_cache


class RegisterForm(forms.Form):
    username = forms.CharField(
        label=_("Username"),
        max_length=50,
        min_length=3,
        widget=forms.TextInput(attrs={"class": "form-control", "id": "username-input"}),
        required=True,
    )

    name = forms.CharField(
        label=_("Name"),
        max_length=150,
        widget=forms.TextInput(attrs={"class": "form-control"}),
        required=True,
    )

    email = forms.EmailField(
        label=_("E-Mail"),
        max_length=150,
        widget=forms.EmailInput(attrs={"class": "form-control"}),
        required=True,
    )

    password = forms.CharField(
        label=_("Password"),
        widget=forms.PasswordInput(
            attrs={"class": "form-control", "id": "password-input"}
        ),
        required=True,
        min_length=8,
        max_length=128,
    )

    error_messages = {
        "username_taken": _(
            "This username is already taken. Please choose a different one."
        ),
        "username_too_short": _("Username must be at least 3 characters long."),
        "username_too_long": _("Username cannot exceed 50 characters."),
        "username_invalid_chars": _(
            "Username can only contain letters, numbers, underscores, and hyphens."
        ),
        "username_invalid_start_end": _(
            "Username cannot start or end with underscore or hyphen."
        ),
        "username_consecutive_special": _(
            "Username cannot contain consecutive special characters."
        ),
        "username_reserved": _("This username is reserved and cannot be used."),
        "email_taken": _("An account with this email already exists."),
        "invalid_email": _("Please enter a valid email address."),
        "password_too_short": _("Password must be at least 8 characters long."),
        "password_too_long": _("Password cannot exceed 128 characters."),
        "password_no_uppercase": _(
            "Password must contain at least one uppercase letter."
        ),
        "password_no_lowercase": _(
            "Password must contain at least one lowercase letter."
        ),
        "password_no_digit": _("Password must contain at least one digit."),
        "password_no_special": _(
            "Password must contain at least one special character (!@#$%^&*()_+-=[]{}|;:,.<>?)."
        ),
        "password_contains_password": _("Password cannot contain the word 'password'."),
        "password_sequential": _(
            "Password cannot contain sequential characters (e.g., 'abcd', '1234')."
        ),
        "password_repeated": _(
            "Password cannot contain repeated characters (e.g., 'aaa', '111')."
        ),
        "network_error": _(
            "Unable to connect to the server. Please check your internet connection and try again."
        ),
        "server_error": _(
            "The server is temporarily unavailable. Please try again later."
        ),
    }

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.user_cache = None

    def clean_username(self):
        username = self.cleaned_data.get("username")
        if not username:
            return username

        # Length validation
        if len(username) < 3:
            raise forms.ValidationError(
                self.error_messages["username_too_short"], code="username_too_short"
            )
        if len(username) > 50:
            raise forms.ValidationError(
                self.error_messages["username_too_long"], code="username_too_long"
            )

        # Character validation
        if not all(c.isalnum() or c in "_-" for c in username):
            raise forms.ValidationError(
                self.error_messages["username_invalid_chars"],
                code="username_invalid_chars",
            )

        # Cannot start or end with special characters
        if username.startswith(("_", "-")) or username.endswith(("_", "-")):
            raise forms.ValidationError(
                self.error_messages["username_invalid_start_end"],
                code="username_invalid_start_end",
            )

        # Cannot contain consecutive special characters
        if any(seq in username for seq in ["__", "--", "_-", "-_"]):
            raise forms.ValidationError(
                self.error_messages["username_consecutive_special"],
                code="username_consecutive_special",
            )

        # Reserved usernames
        reserved = [
            "admin",
            "root",
            "system",
            "api",
            "www",
            "mail",
            "ftp",
            "test",
            "user",
            "support",
            "help",
            "info",
            "null",
            "undefined",
        ]
        if username.lower() in reserved:
            raise forms.ValidationError(
                self.error_messages["username_reserved"], code="username_reserved"
            )

        return username

    def clean_password(self):
        password = self.cleaned_data.get("password")
        if not password:
            return password

        # Length validation
        if len(password) < 8:
            raise forms.ValidationError(
                self.error_messages["password_too_short"], code="password_too_short"
            )
        if len(password) > 128:
            raise forms.ValidationError(
                self.error_messages["password_too_long"], code="password_too_long"
            )

        # Character composition validation
        if not any(c.isupper() for c in password):
            raise forms.ValidationError(
                self.error_messages["password_no_uppercase"],
                code="password_no_uppercase",
            )
        if not any(c.islower() for c in password):
            raise forms.ValidationError(
                self.error_messages["password_no_lowercase"],
                code="password_no_lowercase",
            )
        if not any(c.isdigit() for c in password):
            raise forms.ValidationError(
                self.error_messages["password_no_digit"], code="password_no_digit"
            )

        special_chars = "!@#$%^&*()_+-=[]{}|;:,.<>?"
        if not any(c in special_chars for c in password):
            raise forms.ValidationError(
                self.error_messages["password_no_special"], code="password_no_special"
            )

        # Content restrictions
        if "password" in password.lower():
            raise forms.ValidationError(
                self.error_messages["password_contains_password"],
                code="password_contains_password",
            )

        # Sequential characters check
        for i in range(len(password) - 3):
            substr = password[i : i + 4]
            if self._is_sequential(substr):
                raise forms.ValidationError(
                    self.error_messages["password_sequential"],
                    code="password_sequential",
                )

        # Repeated characters check
        for i in range(len(password) - 2):
            if password[i] == password[i + 1] == password[i + 2]:
                raise forms.ValidationError(
                    self.error_messages["password_repeated"], code="password_repeated"
                )

        return password

    def _is_sequential(self, s):
        """Check if a 4-character string contains sequential characters"""
        if len(s) != 4:
            return False

        # Check for ascending sequence
        ascending = all(ord(s[i]) == ord(s[i - 1]) + 1 for i in range(1, 4))
        # Check for descending sequence
        descending = all(ord(s[i]) == ord(s[i - 1]) - 1 for i in range(1, 4))

        return ascending or descending


class User(object):
    id = None
    pk = None
    username = ""
    is_staff = False
    is_active = False
    is_superuser = False
    is_authenticated = True
    _groups = EmptyManager(Group)
    _user_permissions = EmptyManager(Permission)

    def __init__(self, json=None, session=None):
        if json:
            self.id = json["id"]
            self.username = json["username"]
            self.email = json["email"]
            self.name = json["name"]
            self.session = json["session"]

        if session:
            self.session = session

        self.image = static("dashboard/images/pb.png")

    def __str__(self):
        return self.name

    def __eq__(self, other):
        return isinstance(other, self.__class__)

    def __ne__(self, other):
        return not self.__eq__(other)

    def __hash__(self):
        return self.id

    def save(self):
        raise NotImplementedError(
            "Django doesn't provide a DB representation for User."
        )

    def delete(self):
        raise NotImplementedError(
            "Django doesn't provide a DB representation for User."
        )

    def set_password(self, raw_password):
        raise NotImplementedError(
            "Django doesn't provide a DB representation for User."
        )

    def check_password(self, raw_password):
        raise NotImplementedError(
            "Django doesn't provide a DB representation for User."
        )

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
