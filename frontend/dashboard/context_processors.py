# SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0

def global_variables(request):
    return {
        'projectname' : 'vWorkflow',
        'model' : 'vModel',
        'user' : 'testuser',
        'success' : 'waiting',
        'user': {
            'name': 'testuser',
            'image': 'pb.png',
        }
    }
