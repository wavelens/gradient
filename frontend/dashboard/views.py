from django.shortcuts import render

# Create your views here.
from django.http import HttpResponse

from django.http import HttpResponse
from django.template import loader


def workflow(request):
    return render(request, "dashboard/overview.html")

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
        'projectname' : 'vWorkflow',
        'model' : 'vModel',
        'details_blocks': details_blocks,
        'built_version' : 'Vbuild (x86_64-linux)',
        'status' : 'Vsucceeded',
        'time' : 'V2 months ago in 1s',
        'id' : 'v940',
        'built_name' : 'Vdataset.corpus',
        # 'icon' : 'green-filter'
    }
    return render(request, "dashboard/log.html", context)