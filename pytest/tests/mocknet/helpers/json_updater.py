"""
This script is used to update a JSON file with one or more patch files.
"""

import os
import json
import sys
import argparse


def deep_merge(original: dict, patch: dict) -> dict:
    """Deep merge two dictionaries, recursively merging nested dictionaries.
    
    Args:
        original: The original dictionary to merge into
        patch: The dictionary containing updates to apply
        
    Returns:
        The merged dictionary
    """
    result = original.copy()
    print(f"\nDeep merging:")

    for key, value in patch.items():
        if key not in result:
            print(f"Adding new key '{key}' with value: {json.dumps(value, indent=2)}")
            result[key] = value
            continue

        assert isinstance(result[key], dict) == isinstance(value, dict)
        if isinstance(result[key], dict):
            print(f"Recursively merging key '{key}'")
            result[key] = deep_merge(result[key], value)
        else:
            print(f"Replacing value for key '{key}' with: {json.dumps(value, indent=2)}")
            result[key] = value

    print(f"Merged result: {json.dumps(result, indent=2)}")
    return result


def update_json(original_path: str, patch_paths: list[str]):
    """Update original json with multiple patch json files if they all exist.
    
    Args:
        original_path: Path to the original JSON file
        patch_paths: List of paths to patch JSON files to apply in sequence
    """
    if not os.path.exists(original_path):
        print(f"Error: Original file {original_path} does not exist")
        sys.exit(1)

    for patch_path in patch_paths:
        if not os.path.exists(patch_path):
            print(f"Error: Patch file {patch_path} does not exist")
            sys.exit(1)

    with open(original_path) as f:
        original = json.load(f)
        print(f"\nOriginal content from {original_path}:")
        print(json.dumps(original, indent=2))

    for patch_path in patch_paths:
        with open(patch_path) as f:
            patch = json.load(f)
            print(f"\nApplying patch from {patch_path}")
            # print(json.dumps(patch, indent=2))
            original = deep_merge(original, patch)
            # print(f"\nAfter applying {patch_path}, current content:")
            # print(json.dumps(original, indent=2))

    # TODO: set "gas_limit" to int everywhere once bench.sh is deprecated.
    # Was needed as a workaround for jq 1.6 bigint bug.
    if original_path.endswith('genesis.json'):
        original['gas_limit'] = int(original['gas_limit'])

    with open(original_path, 'w') as f:
        json.dump(original, f, indent=4)
        # print(f"\nFinal content written to {original_path}:")
        # print(json.dumps(original, indent=2))


def main():
    parser = argparse.ArgumentParser(
        description='Update a JSON file with one or more patch files.',
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        'original',
        help='Path to the original JSON file to update',
    )
    parser.add_argument(
        'patches',
        nargs='+',
        help='One or more patch JSON files to apply in sequence',
    )

    args = parser.parse_args()
    update_json(args.original, args.patches)


if __name__ == '__main__':
    main()
