import subprocess
import sys
from pathlib import Path

def main():
    ws = Path('.')
    main_py = ws / 'main.py'
    if not main_py.exists():
        print("FAIL: main.py not created")
        sys.exit(1)

    # Run the produced main.py
    result = subprocess.run([sys.executable, str(main_py)], capture_output=True, text=True)
    output = result.stdout.strip()
    print("Output:", output)

    expected_parts = ["Processed", "avg=", "tags="]
    if all(p in output for p in expected_parts):
        print("PASS: cache-lift basic execution")
        # Note: full token check would be in the harness_turn.json post-processing
        sys.exit(0)
    else:
        print("FAIL: unexpected output")
        sys.exit(1)

if __name__ == '__main__':
    main()
