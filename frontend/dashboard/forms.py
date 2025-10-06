# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django import forms
from django.utils.translation import gettext_lazy as _
from django.core.exceptions import ValidationError
import re

GEEKS_CHOICES = (
    ("1", "One"),
    ("2", "Two"),
    ("3", "Three"),
    ("4", "Four"),
    ("5", "Five"),
)


class NewOrganizationForm(forms.Form):
    name = forms.CharField(
        label="Name",
        required=True,
        min_length=3,
        max_length=50,
        error_messages={
            "required": "Organization name is required.",
            "min_length": "Organization name must be at least 3 characters long.",
            "max_length": "Organization name cannot exceed 50 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    display_name = forms.CharField(
        label="Display Name",
        required=True,
        max_length=100,
        error_messages={
            "required": "Display name is required.",
            "max_length": "Display name cannot exceed 100 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    description = forms.CharField(
        label="Description",
        required=True,
        max_length=500,
        error_messages={
            "required": "Description is required.",
            "max_length": "Description cannot exceed 500 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )

    def clean_name(self):
        name = self.cleaned_data.get("name")
        if name:
            # Check for valid characters
            if not re.match(r"^[a-zA-Z0-9_-]+$", name):
                raise ValidationError(
                    "Organization name can only contain letters, numbers, hyphens, and underscores."
                )
            # Check for reserved names
            reserved_names = ["admin", "api", "www", "app", "system", "root"]
            if name.lower() in reserved_names:
                raise ValidationError(
                    f'The name "{name}" is reserved and cannot be used.'
                )
        return name

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
    organization = forms.CharField(widget=forms.HiddenInput(), required=True)

    name = forms.CharField(
        label="Name",
        required=True,
        min_length=3,
        max_length=50,
        error_messages={
            "required": "Project name is required.",
            "min_length": "Project name must be at least 3 characters long.",
            "max_length": "Project name cannot exceed 50 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    display_name = forms.CharField(
        label="Display Name",
        required=True,
        max_length=100,
        error_messages={
            "required": "Display name is required.",
            "max_length": "Display name cannot exceed 100 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    description = forms.CharField(
        label="Description",
        required=True,
        max_length=500,
        error_messages={
            "required": "Description is required.",
            "max_length": "Description cannot exceed 500 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    repository = forms.CharField(
        label="Repository",
        required=True,
        error_messages={
            "required": "Repository URL is required.",
        },
        widget=forms.TextInput(
            attrs={
                "class": "form-control",
                "placeholder": "e.g., https://github.com/user/repo.git or ssh://git@example.com/repo.git",
            }
        ),
    )
    evaluation_wildcard = forms.CharField(
        label="Wildcard",
        required=True,
        error_messages={
            "required": "Evaluation wildcard is required.",
        },
        help_text="Pattern to match evaluation files (e.g., **/*.test.js)",
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )

    def clean_name(self):
        name = self.cleaned_data.get("name")
        if name:
            if not re.match(r"^[a-zA-Z0-9_-]+$", name):
                raise ValidationError(
                    "Project name can only contain letters, numbers, hyphens, and underscores."
                )
            reserved_names = [
                "admin",
                "api",
                "www",
                "app",
                "system",
                "root",
                "test",
                "tests",
            ]
            if name.lower() in reserved_names:
                raise ValidationError(
                    f'The name "{name}" is reserved and cannot be used.'
                )
        return name

    def clean_repository(self):
        repository = self.cleaned_data.get("repository")
        if repository:
            # Check for basic Git URL patterns (more flexible to support custom Git servers)
            valid_patterns = [
                # HTTPS URLs
                r"^https://[a-zA-Z0-9._-]+/[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
                # HTTP URLs
                r"^http://[a-zA-Z0-9._-]+/[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
                # SSH URLs with ssh:// prefix
                r"^ssh://[a-zA-Z0-9@._-]+:[0-9]+/[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
                # SSH URLs with git:// prefix
                r"^git://[a-zA-Z0-9._-]+/[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
                # SCP-style SSH URLs
                r"^[a-zA-Z0-9._-]+@[a-zA-Z0-9._-]+:[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
                # Git SSH URLs
                r"^git@[a-zA-Z0-9._-]+:[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
            ]

            # Block file:// URLs for security
            if repository.startswith("file://") or repository.startswith("file:"):
                raise ValidationError(
                    "Local file URLs are not allowed for security reasons."
                )

            if not any(re.match(pattern, repository) for pattern in valid_patterns):
                raise ValidationError(
                    "Please enter a valid Git repository URL. Supported formats: https://, http://, ssh://, git://, or SSH SCP-style URLs."
                )
        return repository


class NewServerForm(forms.Form):
    organization = forms.CharField(widget=forms.HiddenInput(), required=False)
    server_name = forms.CharField(
        label="Name",
        required=True,
        min_length=3,
        max_length=50,
        error_messages={
            "required": "Server name is required.",
            "min_length": "Server name must be at least 3 characters long.",
            "max_length": "Server name cannot exceed 50 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    host = forms.CharField(
        label="Host",
        required=True,
        error_messages={
            "required": "Server host is required.",
        },
        help_text="IP address or domain name of the server",
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    port = forms.IntegerField(
        label="Port",
        required=True,
        min_value=1,
        max_value=65535,
        initial=22,
        error_messages={
            "required": "Port number is required.",
            "invalid": "Please enter a valid port number.",
            "min_value": "Port number must be between 1 and 65535.",
            "max_value": "Port number must be between 1 and 65535.",
        },
        widget=forms.NumberInput(
            attrs={"class": "form-control", "min": 1, "max": 65535}
        ),
    )
    username = forms.CharField(
        label="Username",
        required=True,
        min_length=1,
        max_length=32,
        error_messages={
            "required": "Username is required.",
            "min_length": "Username cannot be empty.",
            "max_length": "Username cannot exceed 32 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    architectures = forms.CharField(
        required=True,
        error_messages={
            "required": "At least one architecture must be selected.",
        },
        widget=forms.HiddenInput(attrs={"id": "architectures_hidden"}),
        label="Architectures",
    )
    features = forms.CharField(
        required=True,
        error_messages={
            "required": "At least one feature must be selected.",
        },
        widget=forms.HiddenInput(attrs={"id": "features_hidden"}),
        label="Features",
    )

    def clean_host(self):
        host = self.cleaned_data.get("host")
        if host:
            # Check for valid IP address or domain name
            ip_pattern = r"^(?:[0-9]{1,3}\.){3}[0-9]{1,3}$"
            domain_pattern = r"^[a-zA-Z0-9]([a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?)*$"

            if not (re.match(ip_pattern, host) or re.match(domain_pattern, host)):
                raise ValidationError("Please enter a valid IP address or domain name.")

            # Check for localhost variations
            if host.lower() in ["localhost", "127.0.0.1", "::1"]:
                raise ValidationError(
                    "Localhost addresses are not allowed for remote servers."
                )
        return host

    def clean_architectures(self):
        architectures = self.cleaned_data.get("architectures")
        if architectures:
            arch_list = [
                arch.strip() for arch in architectures.split(",") if arch.strip()
            ]
            if not arch_list:
                raise ValidationError("At least one architecture must be selected.")
            valid_archs = ["x86_64", "arm64", "armv7", "i386", "ppc64le", "s390x"]
            invalid_archs = [arch for arch in arch_list if arch not in valid_archs]
            if invalid_archs:
                raise ValidationError(
                    f'Invalid architectures: {", ".join(invalid_archs)}. Valid options: {", ".join(valid_archs)}.'
                )
        return architectures

    def clean_features(self):
        features = self.cleaned_data.get("features")
        if features:
            feature_list = [
                feat.strip() for feat in features.split(",") if feat.strip()
            ]
            if not feature_list:
                raise ValidationError("At least one feature must be selected.")
        return features


class NewCacheForm(forms.Form):
    name = forms.CharField(
        label="Name",
        required=True,
        min_length=3,
        max_length=50,
        error_messages={
            "required": "Cache name is required.",
            "min_length": "Cache name must be at least 3 characters long.",
            "max_length": "Cache name cannot exceed 50 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    display_name = forms.CharField(
        label="Display Name",
        required=True,
        max_length=100,
        error_messages={
            "required": "Display name is required.",
            "max_length": "Display name cannot exceed 100 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    description = forms.CharField(
        label="Description",
        required=True,
        max_length=500,
        error_messages={
            "required": "Description is required.",
            "max_length": "Description cannot exceed 500 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    priority = forms.IntegerField(
        label="Priority",
        required=True,
        min_value=1,
        max_value=100,
        initial=50,
        error_messages={
            "required": "Priority is required.",
            "invalid": "Please enter a valid priority number.",
            "min_value": "Priority must be between 1 and 100.",
            "max_value": "Priority must be between 1 and 100.",
        },
        help_text="Higher numbers indicate higher priority (1-100)",
        widget=forms.NumberInput(attrs={"class": "form-control", "min": 1, "max": 100}),
    )

    def clean_name(self):
        name = self.cleaned_data.get("name")
        if name:
            if not re.match(r"^[a-zA-Z0-9_-]+$", name):
                raise ValidationError(
                    "Cache name can only contain letters, numbers, hyphens, and underscores."
                )
            reserved_names = [
                "admin",
                "api",
                "www",
                "app",
                "system",
                "root",
                "cache",
                "temp",
            ]
            if name.lower() in reserved_names:
                raise ValidationError(
                    f'The name "{name}" is reserved and cannot be used.'
                )
        return name


class EditServerForm(forms.Form):
    server = forms.CharField(
        label="Server",
        required=True,
        widget=forms.TextInput(attrs={"class": "form-control", "readonly": True}),
    )
    server_name = forms.CharField(
        label="Name",
        required=True,
        min_length=3,
        max_length=50,
        error_messages={
            "required": "Server name is required.",
            "min_length": "Server name must be at least 3 characters long.",
            "max_length": "Server name cannot exceed 50 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    host = forms.CharField(
        label="Host",
        required=True,
        error_messages={
            "required": "Server host is required.",
        },
        help_text="IP address or domain name of the server",
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    port = forms.IntegerField(
        label="Port",
        required=True,
        min_value=1,
        max_value=65535,
        error_messages={
            "required": "Port number is required.",
            "invalid": "Please enter a valid port number.",
            "min_value": "Port number must be between 1 and 65535.",
            "max_value": "Port number must be between 1 and 65535.",
        },
        widget=forms.NumberInput(
            attrs={"class": "form-control", "min": 1, "max": 65535}
        ),
    )
    username = forms.CharField(
        label="Username",
        required=True,
        min_length=1,
        max_length=32,
        error_messages={
            "required": "Username is required.",
            "min_length": "Username cannot be empty.",
            "max_length": "Username cannot exceed 32 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    architectures = forms.CharField(
        required=True,
        error_messages={
            "required": "At least one architecture must be specified.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
        label="Architectures",
    )
    features = forms.CharField(
        required=True,
        error_messages={
            "required": "At least one feature must be specified.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
        label="Features",
    )


class EditOrganizationForm(forms.Form):
    name = forms.CharField(
        label="Name",
        required=True,
        min_length=3,
        max_length=50,
        error_messages={
            "required": "Organization name is required.",
            "min_length": "Organization name must be at least 3 characters long.",
            "max_length": "Organization name cannot exceed 50 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    display_name = forms.CharField(
        label="Display Name",
        required=True,
        max_length=100,
        error_messages={
            "required": "Display name is required.",
            "max_length": "Display name cannot exceed 100 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    description = forms.CharField(
        label="Description",
        required=True,
        max_length=500,
        error_messages={
            "required": "Description is required.",
            "max_length": "Description cannot exceed 500 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )

    def clean_name(self):
        name = self.cleaned_data.get("name")
        if name:
            if not re.match(r"^[a-zA-Z0-9_-]+$", name):
                raise ValidationError(
                    "Organization name can only contain letters, numbers, hyphens, and underscores."
                )
        return name


class EditProjectForm(forms.Form):
    name = forms.CharField(
        label="Name",
        required=True,
        min_length=3,
        max_length=50,
        error_messages={
            "required": "Project name is required.",
            "min_length": "Project name must be at least 3 characters long.",
            "max_length": "Project name cannot exceed 50 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    display_name = forms.CharField(
        label="Display Name",
        required=True,
        max_length=100,
        error_messages={
            "required": "Display name is required.",
            "max_length": "Display name cannot exceed 100 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    description = forms.CharField(
        label="Description",
        required=True,
        max_length=500,
        error_messages={
            "required": "Description is required.",
            "max_length": "Description cannot exceed 500 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    repository = forms.CharField(
        label="Repository",
        required=True,
        error_messages={
            "required": "Repository URL is required.",
        },
        widget=forms.TextInput(
            attrs={
                "class": "form-control",
                "placeholder": "e.g., https://github.com/user/repo.git or ssh://git@example.com/repo.git",
            }
        ),
    )
    evaluation_wildcard = forms.CharField(
        label="Wildcard",
        required=True,
        error_messages={
            "required": "Evaluation wildcard is required.",
        },
        help_text="Pattern to match evaluation files (e.g., **/*.test.js)",
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )

    def clean_name(self):
        name = self.cleaned_data.get("name")
        if name:
            if not re.match(r"^[a-zA-Z0-9_-]+$", name):
                raise ValidationError(
                    "Project name can only contain letters, numbers, hyphens, and underscores."
                )
        return name

    def clean_repository(self):
        repository = self.cleaned_data.get("repository")
        if repository:
            # Check for basic Git URL patterns (more flexible to support custom Git servers)
            valid_patterns = [
                # HTTPS URLs
                r"^https://[a-zA-Z0-9._-]+/[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
                # HTTP URLs
                r"^http://[a-zA-Z0-9._-]+/[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
                # SSH URLs with ssh:// prefix
                r"^ssh://[a-zA-Z0-9@._-]+:[0-9]+/[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
                # SSH URLs with git:// prefix
                r"^git://[a-zA-Z0-9._-]+/[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
                # SCP-style SSH URLs
                r"^[a-zA-Z0-9._-]+@[a-zA-Z0-9._-]+:[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
                # Git SSH URLs
                r"^git@[a-zA-Z0-9._-]+:[a-zA-Z0-9._/-]+/[a-zA-Z0-9._-]+(\.git)?/?$",
            ]

            # Block file:// URLs for security
            if repository.startswith("file://") or repository.startswith("file:"):
                raise ValidationError(
                    "Local file URLs are not allowed for security reasons."
                )

            if not any(re.match(pattern, repository) for pattern in valid_patterns):
                raise ValidationError(
                    "Please enter a valid Git repository URL. Supported formats: https://, http://, ssh://, git://, or SSH SCP-style URLs."
                )
        return repository


class EditCacheForm(forms.Form):
    name = forms.CharField(
        label="Name",
        required=True,
        min_length=3,
        max_length=50,
        error_messages={
            "required": "Cache name is required.",
            "min_length": "Cache name must be at least 3 characters long.",
            "max_length": "Cache name cannot exceed 50 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    display_name = forms.CharField(
        label="Display Name",
        required=True,
        max_length=100,
        error_messages={
            "required": "Display name is required.",
            "max_length": "Display name cannot exceed 100 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    description = forms.CharField(
        label="Description",
        required=True,
        max_length=500,
        error_messages={
            "required": "Description is required.",
            "max_length": "Description cannot exceed 500 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    priority = forms.IntegerField(
        label="Priority",
        required=True,
        min_value=1,
        max_value=100,
        error_messages={
            "required": "Priority is required.",
            "invalid": "Please enter a valid priority number.",
            "min_value": "Priority must be between 1 and 100.",
            "max_value": "Priority must be between 1 and 100.",
        },
        help_text="Higher numbers indicate higher priority (1-100)",
        widget=forms.NumberInput(attrs={"class": "form-control", "min": 1, "max": 100}),
    )

    def clean_name(self):
        name = self.cleaned_data.get("name")
        if name:
            if not re.match(r"^[a-zA-Z0-9_-]+$", name):
                raise ValidationError(
                    "Cache name can only contain letters, numbers, hyphens, and underscores."
                )
        return name


class EditUserForm(forms.Form):
    name = forms.CharField(
        label="Full Name",
        required=True,
        min_length=2,
        max_length=100,
        error_messages={
            "required": "Full name is required.",
            "min_length": "Name must be at least 2 characters long.",
            "max_length": "Name cannot exceed 100 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    username = forms.CharField(
        label="Username",
        required=True,
        min_length=3,
        max_length=30,
        error_messages={
            "required": "Username is required.",
            "min_length": "Username must be at least 3 characters long.",
            "max_length": "Username cannot exceed 30 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    email = forms.EmailField(
        label="Email",
        required=True,
        error_messages={
            "required": "Email address is required.",
            "invalid": "Please enter a valid email address.",
        },
        widget=forms.EmailInput(attrs={"class": "form-control"}),
    )

    def clean_name(self):
        name = self.cleaned_data.get("name")
        if name:
            # Check for valid name format (letters, spaces, hyphens, apostrophes)
            if not re.match(r"^[a-zA-Z\s\-\']+$", name):
                raise ValidationError(
                    "Name can only contain letters, spaces, hyphens, and apostrophes."
                )
        return name

    def clean_username(self):
        username = self.cleaned_data.get("username")
        if username:
            if not re.match(r"^[a-zA-Z0-9_-]+$", username):
                raise ValidationError(
                    "Username can only contain letters, numbers, hyphens, and underscores."
                )
            if username.lower() in [
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
            ]:
                raise ValidationError(
                    f'The username "{username}" is reserved and cannot be used.'
                )
        return username


class AddOrganizationMemberForm(forms.Form):
    ROLE_CHOICES = [
        ("Admin", "Admin"),
        ("Write", "Write"),
        ("View", "View"),
    ]

    user = forms.CharField(
        label="Username",
        required=True,
        min_length=3,
        max_length=150,
        error_messages={
            "required": "Username is required.",
            "min_length": "Username must be at least 3 characters long.",
            "max_length": "Username cannot exceed 150 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    role = forms.ChoiceField(
        label="Role",
        choices=ROLE_CHOICES,
        required=True,
        error_messages={
            "required": "Please select a role.",
            "invalid_choice": "Please select a valid role.",
        },
        widget=forms.Select(attrs={"class": "form-control"}),
    )

    def clean_user(self):
        user = self.cleaned_data.get("user")
        if user:
            user = user.strip()
            if not user:
                raise ValidationError("Username cannot be empty.")
            if " " in user:
                raise ValidationError("Username cannot contain spaces.")
            if not user.replace("_", "").replace("-", "").replace(".", "").isalnum():
                raise ValidationError(
                    "Username can only contain letters, numbers, underscores, hyphens, and periods."
                )
            # Check for reserved usernames
            reserved_usernames = [
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
            if user.lower() in reserved_usernames:
                raise ValidationError(
                    f'The username "{user}" is reserved and cannot be used.'
                )
        return user


class EditOrganizationMemberForm(forms.Form):
    ROLE_CHOICES = [
        ("admin", "Admin"),
        ("member", "Member"),
        ("viewer", "Viewer"),
    ]

    role = forms.ChoiceField(
        label="Role",
        choices=ROLE_CHOICES,
        required=True,
        error_messages={
            "required": "Please select a role.",
            "invalid_choice": "Please select a valid role.",
        },
        widget=forms.Select(attrs={"class": "form-control"}),
    )


class AddOrganizationServerForm(forms.Form):
    name = forms.CharField(
        label="Server Name",
        required=True,
        min_length=3,
        max_length=50,
        error_messages={
            "required": "Server name is required.",
            "min_length": "Server name must be at least 3 characters long.",
            "max_length": "Server name cannot exceed 50 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    display_name = forms.CharField(
        label="Display Name",
        required=True,
        max_length=100,
        error_messages={
            "required": "Display name is required.",
            "max_length": "Display name cannot exceed 100 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    host = forms.CharField(
        label="Host",
        required=True,
        error_messages={
            "required": "Server host is required.",
        },
        help_text="IP address or domain name of the server",
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    port = forms.IntegerField(
        label="Port",
        required=True,
        min_value=1,
        max_value=65535,
        initial=22,
        error_messages={
            "required": "Port number is required.",
            "invalid": "Please enter a valid port number.",
            "min_value": "Port number must be between 1 and 65535.",
            "max_value": "Port number must be between 1 and 65535.",
        },
        widget=forms.NumberInput(
            attrs={"class": "form-control", "min": 1, "max": 65535}
        ),
    )
    username = forms.CharField(
        label="Username",
        required=True,
        min_length=1,
        max_length=32,
        error_messages={
            "required": "Username is required.",
            "min_length": "Username cannot be empty.",
            "max_length": "Username cannot exceed 32 characters.",
        },
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    architectures = forms.CharField(
        label="Architectures",
        required=True,
        error_messages={
            "required": "At least one architecture must be specified.",
        },
        help_text="Comma-separated list of supported architectures",
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )
    features = forms.CharField(
        label="Features",
        required=True,
        error_messages={
            "required": "At least one feature must be specified.",
        },
        help_text="Comma-separated list of available features",
        widget=forms.TextInput(attrs={"class": "form-control"}),
    )

    def clean_name(self):
        name = self.cleaned_data.get("name")
        if name:
            if not re.match(r"^[a-zA-Z0-9_-]+$", name):
                raise ValidationError(
                    "Server name can only contain letters, numbers, hyphens, and underscores."
                )
        return name

    def clean_host(self):
        host = self.cleaned_data.get("host")
        if host:
            # Check for valid IP address or domain name
            ip_pattern = r"^(?:[0-9]{1,3}\.){3}[0-9]{1,3}$"
            domain_pattern = r"^[a-zA-Z0-9]([a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?)*$"

            if not (re.match(ip_pattern, host) or re.match(domain_pattern, host)):
                raise ValidationError("Please enter a valid IP address or domain name.")

            if host.lower() in ["localhost", "127.0.0.1", "::1"]:
                raise ValidationError(
                    "Localhost addresses are not allowed for remote servers."
                )
        return host
