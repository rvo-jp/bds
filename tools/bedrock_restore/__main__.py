import argparse

from .restore import build

parser = argparse.ArgumentParser(
    description="Build an offline candidate inside a verified-copy workspace; never deploy it"
)
parser.add_argument("workspace")
parser.add_argument(
    "plan",
    help="Private JSON plan with explicit player bindings and missing-chunk policy",
)
args = parser.parse_args()
build(args.workspace, args.plan)
