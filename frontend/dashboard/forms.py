# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django import forms

from django import forms

class LoginForm(forms.Form):
    username = forms.CharField(
        label='E-Mail-Adresse oder Benutzername',
        max_length=150,
        widget=forms.TextInput(attrs={
            'class': 'form-control',
        }),
        required=True
    )
    password = forms.CharField(
        label='Passwort',
        widget=forms.PasswordInput(attrs={
            'class': 'form-control',
        }),
        required=True
    )
    remember_me = forms.BooleanField(
        label='Dieses Gerät Speichern',
        required=False,
        widget=forms.CheckboxInput(attrs={
            'class': 'form-check-input'
        })
    )
