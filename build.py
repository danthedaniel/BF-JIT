#!/usr/bin/env python3

import subprocess
import sys
import shutil
from typing import Dict, List


TARGETS = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "i686-unknown-linux-gnu",
]


def check_command(command: str) -> bool:
    """Check if a command is available in PATH."""
    return shutil.which(command) is not None


def run_command(command: List[str]) -> bool:
    """Run a build command and return True if successful."""
    try:
        result = subprocess.run(
            command,
            capture_output=False,
            check=True
        )
        return result.returncode == 0
    except subprocess.CalledProcessError:
        return False


def main():
    build_results: Dict[str, str] = {}

    # Check prerequisites
    if not check_command("cargo"):
        print("cargo could not be found, install it first", file=sys.stderr)
        print('curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh', file=sys.stderr)
        sys.exit(1)

    if not check_command("cross"):
        print("cross could not be found, install it first", file=sys.stderr)
        print("cargo install cross --git https://github.com/cross-rs/cross", file=sys.stderr)
        sys.exit(1)

    for target in TARGETS:
        if "linux" in target:
            success = run_command(["cross", "build", "--target", target])
            build_results[target] = "SUCCESS" if success else "FAILED"
        elif "apple" in target:
            success = run_command(["cargo", "build", "--target", target])
            build_results[target] = "SUCCESS" if success else "FAILED"
        else:
            build_results[target] = "FAILED (unknown target)"

    print(f"{'Target':<30} {'Build Result':<15}")
    print("-" * 45)
    for target in TARGETS:
        print(f"{target:<30} {build_results.get(target, 'N/A'):<15}")


if __name__ == "__main__":
    main()
