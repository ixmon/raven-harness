import subprocess
import sys
from pathlib import Path

def main():
    ws = Path('.')
    main_py = ws / 'main.py'
    if not main_py.exists():
        print("FAIL: main.py not created")
        sys.exit(1)

    result = subprocess.run([sys.executable, str(main_py)], capture_output=True, text=True)
    output = result.stdout.strip().lower()
    print("Output:", output)

    # After patch in the flow, should reflect threshold=5 / loose
    if 'threshold: 5' in output or 'loose' in output or 'processed with loose' in output:
        print("PASS: cache-fidelity - used updated config after summary + patch/invalidate")
        sys.exit(0)
    else:
        print("FAIL: did not reflect updated config (possible stale summary use)")
        sys.exit(1)

if __name__ == '__main__':
    main()
