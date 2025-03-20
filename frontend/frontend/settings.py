# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

"""
Django settings for frontend project.

Generated by 'django-admin startproject' using Django 4.2.14.

For more information on this file, see
https://docs.djangoproject.com/en/4.2/topics/settings/

For the full list of settings and their values, see
https://docs.djangoproject.com/en/4.2/ref/settings/
"""

import os
from pathlib import Path
from django.utils.translation import gettext_lazy

import sentry_sdk
from sentry_sdk.integrations.django import DjangoIntegration

# Build paths inside the project like this: BASE_DIR / 'subdir'.
BASE_DIR = Path(__file__).resolve().parent.parent


# Quick-start development settings - unsuitable for production
# See https://docs.djangoproject.com/en/4.2/howto/deployment/checklist/

SECRET_KEY_FILE = os.environ.get('GRADIENT_CRYPT_SECRET_FILE', None)
SECRET_KEY = open(SECRET_KEY_FILE).read().strip() if SECRET_KEY_FILE else 'django-insecure-8aa0=fofed&)*3(3v)b39@vm@zxsqss=#5asynz9ru4=zm6$e6'

DEBUG = os.environ.get('GRADIENT_DEBUG', 'true') == 'true'

# TODO: Is this necessary?
ALLOWED_HOSTS = ['*']

CSRF_TRUSTED_ORIGINS = [
    os.environ.get('GRADIENT_SERVE_URL', 'http://127.0.0.1:8000')
]

# Application definition

INSTALLED_APPS = [
    'whitenoise.runserver_nostatic',
    'django.contrib.auth',
    'django.contrib.contenttypes',
    'django.contrib.sessions',
    'django.contrib.messages',
    'django.contrib.staticfiles',
    'dashboard',
]

MIDDLEWARE = [
    'django.middleware.security.SecurityMiddleware',
    'whitenoise.middleware.WhiteNoiseMiddleware',
    'django.contrib.sessions.middleware.SessionMiddleware',
    'django.middleware.common.CommonMiddleware',
    'django.middleware.csrf.CsrfViewMiddleware',
    'dashboard.auth.AuthenticationMiddleware',
    'django.contrib.messages.middleware.MessageMiddleware',
    'django.middleware.clickjacking.XFrameOptionsMiddleware',
]

ROOT_URLCONF = 'frontend.urls'

TEMPLATES = [
    {
        'BACKEND': 'django.template.backends.django.DjangoTemplates',
        'DIRS': [],
        'APP_DIRS': True,
        'OPTIONS': {
            'context_processors': [
                'django.template.context_processors.debug',
                'django.template.context_processors.request',
                'django.contrib.auth.context_processors.auth',
                'django.contrib.messages.context_processors.messages',
                'dashboard.context_processors.global_variables',
            ],
        },
    },
]

WSGI_APPLICATION = 'frontend.wsgi.application'
ASGI_APPLICATION = 'frontend.asgi.application'


# Database
# https://docs.djangoproject.com/en/4.2/ref/settings/#databases

DATABASES = {
    'default': {
        'ENGINE': 'django.db.backends.sqlite3',
        'NAME': Path(os.environ.get('GRADIENT_BASE_PATH', BASE_DIR)) / 'db.sqlite3',
    }
}



# Password validation
# https://docs.djangoproject.com/en/4.2/ref/settings/#auth-password-validators

AUTH_PASSWORD_VALIDATORS = [
    {
        'NAME': 'django.contrib.auth.password_validation.UserAttributeSimilarityValidator',
    },
    {
        'NAME': 'django.contrib.auth.password_validation.MinimumLengthValidator',
    },
    {
        'NAME': 'django.contrib.auth.password_validation.CommonPasswordValidator',
    },
    {
        'NAME': 'django.contrib.auth.password_validation.NumericPasswordValidator',
    },
]


# Internationalization
# https://docs.djangoproject.com/en/4.2/topics/i18n/

LANGUAGE_CODE = 'en-us'

TIME_ZONE = 'UTC'

USE_I18N = True

USE_TZ = True

LANGUAGES = (
    ("en", gettext_lazy("English")),
)

PARLER_LANGUAGES = {
    None: (
        {'code': 'en'},
    ),
    'default': {
        'fallback': 'en',
        'hide_untranslated': False,
    }
}

LOCALE_PATHS = [
    BASE_DIR / "locale/",
]

# Static files (CSS, JavaScript, Images)
# https://docs.djangoproject.com/en/4.2/howto/static-files/

STATIC_URL = 'static/'
STATIC_ROOT = BASE_DIR / "static"

STORAGES = {
    "staticfiles": {
        "BACKEND": "whitenoise.storage.CompressedManifestStaticFilesStorage",
    },
}

# Default primary key field type
# https://docs.djangoproject.com/en/4.2/ref/settings/#default-auto-field

DEFAULT_AUTO_FIELD = 'django.db.models.BigAutoField'

# Gradient Api

GRADIENT_BASE_URL = f"{os.environ.get('GRADIENT_API_URL', 'http://127.0.0.1:3000')}/api/v1"
GRADIENT_DISABLE_REGISTRATION = os.environ.get('GRADIENT_DISABLE_REGISTRATION', 'false') == 'true'
GRADIENT_OAUTH_REQUIRED = os.environ.get('GRADIENT_OAUTH_REQUIRED', 'false') == 'true'

# Authentication

LOGIN_REDIRECT_URL = '/'
LOGIN_URL = '/account/login'

# Sentry

if os.environ.get('GRADIENT_REPORT_ERRORS', 'false') == 'true':
    sentry_sdk.init(
        dsn="https://93dbad33e86147dcac5230b2ba7764a2@reports.wavelens.io/2",
        integrations=[DjangoIntegration()],
        auto_session_tracking=False,
        traces_sample_rate=0
    )
