CONFIG = {
    'threshold': 10,
    'mode': 'strict',
    'version': '1.0'
}

def get_setting(key):
    return CONFIG.get(key)

def process_value(v):
    if CONFIG['mode'] == 'strict' and v < CONFIG['threshold']:
        return 'low'
    return 'ok'
