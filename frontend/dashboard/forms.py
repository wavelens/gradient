# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django import forms
from django.utils.translation import gettext_lazy as _

GEEKS_CHOICES =(
    ("1", "One"),
    ("2", "Two"),
    ("3", "Three"),
    ("4", "Four"),
    ("5", "Five"),
)

class NewOrganizationForm(forms.Form):
    # owner = forms.ChoiceField(
    #     choices=GEEKS_CHOICES,
    #     required=True,
    #     widget=forms.Select,
    #     label='Besitzer'
    # )
    name = forms.CharField(
        label='Organisations-Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    description = forms.CharField(
        label='Beschreibung',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    # show = forms.BooleanField(
    #     label='In privates Repository umwandeln',
    #     required=False,
    #     widget=forms.CheckboxInput(attrs={
    #         'class': 'form-check-input'
    #     })
    # )
    # description = forms.CharField(
    #     label="Beschreibung",
    #     required=False,
    #     widget=forms.Textarea(attrs={
    #         'placeholder': 'Gib eine kurze Beschreibung an (optional)',
    #         'rows': 4,
    #     })
    # )
    # template = forms.CharField(
    #     label='Template',
    #     required=False,
    #     widget=forms.TextInput(attrs={
    #         'type': 'search',
    #         'class': 'form-control',
    #         'placeholder': 'Vorlage auswählen'
    #     })
    # )
    # issue_label = forms.CharField(
    #     label='Issue Label',
    #     required=False,
    #     widget=forms.TextInput(attrs={
    #         'class': 'form-control',
    #         'placeholder': 'Wähle ein Issue-Label-Set.'
    #     })
    # )
    # gitignore = forms.CharField(
    #     label='.gitignore',
    #     required=False,
    #     widget=forms.TextInput(attrs={
    #         'type': 'search',
    #         'class': 'form-control',
    #         'placeholder': 'Wähle eine .gitignore-Vorlage aus.'
    #     })
    # )
    # license = forms.CharField(
    #     label='Lizenz',
    #     required=False,
    #     widget=forms.TextInput(attrs={
    #         'type': 'search',
    #         'class': 'form-control',
    #         'placeholder': 'Wähle eine Lizenz aus.'
    #     })
    # )
    # readme = forms.CharField(
    #     label='README',
    #     required=False,
    #     widget=forms.TextInput(attrs={
    #         'type': 'search',
    #         'class': 'form-control',
    #     })
    # )
    # initialisieren = forms.BooleanField(
    #     label='Repository initalisieren (Fügt .gitignore, License und README-Dateien hinzu)',
    #     required=False,
    #     widget=forms.CheckboxInput(attrs={
    #         'class': 'form-check-input'
    #     })
    # )
    # branch = forms.CharField(
    #     label='Standardbranch',
    #     required=False,
    #     widget=forms.TextInput(attrs={
    #         'class': 'form-control',
    #         'value': 'main'
    #     })
    # )
    # format = forms.CharField(
    #     label='Objektformat',
    #     required=False,
    #     widget=forms.TextInput(attrs={
    #         'type': 'search',
    #         'class': 'form-control',
    #         'placeholder': 'sha1'
    #     })
    # )
    # template_check = forms.BooleanField(
    #     label='Repository zu einem Template machen',
    #     required=False,
    #     widget=forms.CheckboxInput(attrs={
    #         'class': 'form-check-input'
    #     })
    # )

class NewProjectForm(forms.Form):
    name = forms.CharField(
        label='Projekt-Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    description = forms.CharField(
        label='Beschreibung',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    repository = forms.ChoiceField(
        required=True,
        widget=forms.Select,
        label='Repository'
    )

class NewServerForm(forms.Form):
    organization_id = forms.CharField(
        label='Organization',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    server_name = forms.CharField(
        label='Server-Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    host = forms.CharField(
        label='Host',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    port = forms.ChoiceField(
        choices=GEEKS_CHOICES,
        required=True,
        widget=forms.Select,
        label='Port'
    )
    username = forms.ChoiceField(
        choices=GEEKS_CHOICES,
        required=True,
        widget=forms.Select,
        label='Username'
    )
    architectures = forms.ChoiceField(
        choices=GEEKS_CHOICES,
        required=True,
        widget=forms.Select,
        label='Architectures'
    )
    features = forms.ChoiceField(
        choices=GEEKS_CHOICES,
        required=True,
        widget=forms.Select,
        label='Features'
    )
