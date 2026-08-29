import json
import mesonbuild.interpreter
from mesonbuild.options import OptionStore
from mesonbuild.optinterpreter import OptionInterpreter

def parse(path):
    try:
        store = OptionStore(False)
    except TypeError:
        store = OptionStore()
    oi = OptionInterpreter(store, '')
    oi.process(path)
    return json.dumps([{
        'name': str(k),
        'kind': type(v).__name__,
        'value': v.value,
        'description': v.description,
        'choices': getattr(v, 'choices', None),
        'deprecated': bool(getattr(v, 'deprecated', False)),
    } for k, v in oi.options.items()], default=str)
