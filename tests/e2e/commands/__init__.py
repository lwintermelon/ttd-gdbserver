"""One module per `e2e.py` subcommand.

Each module exposes `add_arguments(parser)` and `main(args) -> int` and
contains only what is specific to its gate; everything shared lives in
`harness/`.
"""
