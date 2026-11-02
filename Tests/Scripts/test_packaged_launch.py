"""The legacy GUI launch check is not safe for the offline fork."""

import sys


if __name__ == "__main__":
    sys.exit("Packaged launch test is disabled: it starts a GUI and can scan real files. "
             "Offline builds no longer create the update acknowledgement it expected. "
             "Manual GUI verification in an isolated environment is still required.")
