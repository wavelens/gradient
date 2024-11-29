def global_variables(request):
    return {
        'projectname' : 'vWorkflow',
        'model' : 'vModel',
        'user' : 'testuser',
        'user': {
            'name': 'testuser',
            'image': 'pb.png',
        }
    }