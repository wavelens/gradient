# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
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
        label='Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    display_name = forms.CharField(
        label='Display Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    description = forms.CharField(
        label='Description',
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
    organization = forms.ChoiceField(
        label='Organization',
        required=True,
        widget=forms.Select
    )

    name = forms.CharField(
        label='Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    display_name = forms.CharField(
        label='Display Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    description = forms.CharField(
        label='Description',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    repository = forms.CharField(
        label='Repository',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    evaluation_wildcard = forms.CharField(
        label='Wildcard',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )

class NewServerForm(forms.Form):
    organization = forms.CharField(
        label='Organization',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    server_name = forms.CharField(
        label='Name',
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
    port = forms.IntegerField(
        label='Port',
        required=True,
        widget=forms.NumberInput(attrs={
            'class': 'form-control',
        })
    )
    username = forms.CharField(
        label='Username',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    architectures = forms.CharField(
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        }),
        label='Architectures'
    )
    features = forms.CharField(
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        }),
        label='Features'
    )

class NewCacheForm(forms.Form):
    name = forms.CharField(
        label='Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    display_name = forms.CharField(
        label='Display Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    description = forms.CharField(
        label='Description',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    priority = forms.IntegerField(
        label='Priority',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control',
        })
    )

class EditServerForm(forms.Form):
    server = forms.CharField(
        label='Server',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    server_name = forms.CharField(
        label='Name',
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
    port = forms.IntegerField(
        label='Port',
        required=True,
        widget=forms.NumberInput(attrs={
            'class': 'form-control',
        })
    )
    username = forms.CharField(
        label='Username',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    architectures = forms.CharField(
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        }),
        label='Architectures'
    )
    features = forms.CharField(
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        }),
        label='Features'
    )

class EditOrganizationForm(forms.Form):
    name = forms.CharField(
        label='Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    display_name = forms.CharField(
        label='Display Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    description = forms.CharField(
        label='Description',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    
class EditProjectForm(forms.Form):
    name = forms.CharField(
        label='Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    display_name = forms.CharField(
        label='Display Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    description = forms.CharField(
        label='Description',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    repository = forms.CharField(
        label='Repository',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    evaluation_wildcard = forms.CharField(
        label='Wildcard',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )

class EditCacheForm(forms.Form):
    name = forms.CharField(
        label='Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    display_name = forms.CharField(
        label='Display Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    description = forms.CharField(
        label='Description',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    priority = forms.IntegerField(
        label='Priority',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control',
        })
    )

class EditUserForm(forms.Form):
    name = forms.CharField(
        label='Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    username = forms.CharField(
        label='Username',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    email = forms.CharField(
        label='Email',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )

class AddOrganizationMemberForm(forms.Form):
    ROLE_CHOICES = [
        ('Admin', 'Admin'),
        ('Write', 'Write'),
        ('View', 'View'),
    ]
    
    user = forms.CharField(
        label='Username',
        required=True,
        min_length=3,
        max_length=150,
        widget=forms.TextInput(attrs={
            'class': 'form-control',
            'placeholder': 'Enter username'
        })
    )
    role = forms.ChoiceField(
        label='Role',
        choices=ROLE_CHOICES,
        required=True,
        widget=forms.Select(attrs={
            'class': 'form-control'
        })
    )
    
    def clean_user(self):
        user = self.cleaned_data.get('user')
        if user:
            user = user.strip()
            if not user:
                raise forms.ValidationError("Username cannot be empty.")
            if ' ' in user:
                raise forms.ValidationError("Username cannot contain spaces.")
            if not user.replace('_', '').replace('-', '').replace('.', '').isalnum():
                raise forms.ValidationError("Username can only contain letters, numbers, underscores, hyphens, and periods.")
        return user

class EditOrganizationMemberForm(forms.Form):
    ROLE_CHOICES = [
        ('admin', 'Admin'),
        ('member', 'Member'),
        ('viewer', 'Viewer'),
    ]
    
    role = forms.ChoiceField(
        label='Role',
        choices=ROLE_CHOICES,
        required=True,
        widget=forms.Select(attrs={
            'class': 'form-control'
        })
    )

class AddOrganizationServerForm(forms.Form):
    name = forms.CharField(
        label='Server Name',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    display_name = forms.CharField(
        label='Display Name',
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
    port = forms.IntegerField(
        label='Port',
        required=True,
        widget=forms.NumberInput(attrs={
            'class': 'form-control'
        })
    )
    username = forms.CharField(
        label='Username',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    architectures = forms.CharField(
        label='Architectures',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )
    features = forms.CharField(
        label='Features',
        required=True,
        widget=forms.TextInput(attrs={
            'class': 'form-control'
        })
    )

