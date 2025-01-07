# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from django.shortcuts import render
from django.http import HttpResponse
from django.template import loader
from django.shortcuts import render
from .forms import LoginForm


def workflow(request):
    details_blocks = [
        {
            'project': "build",
            'id': "wvls:build",
            'exec': 34,
            'duration': '12m 11s',
            'performance': 'filter',
            'latest_runs': 'filter',
            'latestRuns': {
                '1': 'true',
                '2': 'true',
                '3': 'false',
                '4': 'true',
                '5': 'true',
            },
            'wfp': {
                '1': 'true',
                '2': 'false',
                '3': 'nothing',
            }
        },
        {
            'project': "build2",
            'id': "wvls:build",
            'exec': 34,
            'duration': '12m 11s',
            'performance': 'filter',
            'latest_runs': 'filter',
            'latestRuns': {
                '1': 'false',
                '2': 'true',
                '3': 'false',
                '4': 'true',
                '5': 'true',
            },
            'wfp': {
                '1': 'true',
                '2': 'false',
                '3': 'nothing',
            }
        },
        {
            'project': "build3",
            'id': "wvls:build",
            'exec': 34,
            'duration': '12m 11s',
            'performance': 'filter',
            'latest_runs': 'filter',
            'latestRuns': {
                '1': 'false',
                '2': 'false',
                '3': 'false',
                '4': 'true',
                '5': 'true',
            },
            'wfp': {
                '1': 'true',
                '2': 'false',
                '3': 'nothing',
            }
        },
        {
            'project': "build4",
            'id': "wvls:build",
            'exec': 34,
            'duration': '12m 11s',
            'performance': 'filter',
            'latest_runs': 'filter',
            'latestRuns': {
                '1': 'nothing',
                '2': 'nothing',
                '3': 'false',
                '4': 'true',
                '5': 'true',
            },
            'wfp': {
                '1': 'true',
                '2': 'false',
                '3': 'nothing',
            }
        },
    ]
    context = {
        'details_blocks': details_blocks
    }
    return render(request, "dashboard/overview.html", context)

def log(request):
    details_blocks = [
        {
            'summary': "Lorem ipsum dolor sit amet, consetetur sadipscing elitr",
            'details': [
                "Lorem ipsum dolor sit amet, consetetur sadipscing elitr",
                "Sed diam nonumy eirmod tempor invidunt ut labore et dolore magna",
                "Aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores",
                "Et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet.",
                "Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua.",
                "At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet."
            ]
        },
        {
            'summary': "Summary 2",
            'details': [
                "Detail 2-1",
                "Detail 2-2",
                "Detail 2-3"
            ]
        }
    ]
    context = {
        'details_blocks': details_blocks,
        'built_version' : 'Vbuild (x86_64-linux)',
        'status' : 'Vsucceeded',
        'time' : 'V2',
        'duration' : '1s',
        'id' : 'v940',
        'built_name' : 'Vdataset.corpus',
        'triggerArt' : 'schedule',
        'triggerTime' : '8 months',
        'git' : 'f72bjds',
        'branch' : 'main',
        'artifacts' : '-',
        # 'icon' : 'green-filter'
    }
    return render(request, "dashboard/log.html", context)

def download(request):
    files = [
    {
        'file': "File 1",
        'type': "dataset",
        'link' : "dataset.zip",
        'actions' : "Details"
    },
    {
        'file': "File 2",
        'type': "dataset",
        'link' : "dataset.zip",
        'actions' : "Details"
    },
    {
        'file': "File 3",
        'type': "dataset",
        'link' : "dataset.zip",
        'actions' : "Details"
    },
    ]
    context = {
        'files': files,
    }
    return render(request, "dashboard/download.html", context)

def model(request):
    models = [
    {
        'name': "Model 1",
        'description': "bliblablubs"
    },
    {
        'name': "Model 2",
        'description': "hihaho"
    }
    ]
    context = {
        'models': models,
    }
    return render(request, "dashboard/model.html", context)

def login(request):
    form = LoginForm()
    return render(request, "dashboard/login.html", {'form': form})
