def parse_data(data):
    """Parse input data into structured records."""
    lines = data.strip().split('\n')
    records = []
    for line in lines:
        if line:
            parts = line.split(',')
            records.append({'id': int(parts[0]), 'value': float(parts[1]), 'tag': parts[2]})
    return records

def filter_records(records, min_value):
    """Filter records above a threshold."""
    return [r for r in records if r['value'] > min_value]

def compute_stats(records):
    """Compute basic stats on the filtered records."""
    if not records:
        return {'count': 0, 'avg': 0, 'tags': []}
    values = [r['value'] for r in records]
    tags = list(set(r['tag'] for r in records))
    return {
        'count': len(records),
        'avg': sum(values) / len(values),
        'tags': sorted(tags)
    }

def format_report(stats):
    """Format the final report."""
    return f"Processed {stats['count']} records, avg={stats['avg']:.2f}, tags={stats['tags']}"
