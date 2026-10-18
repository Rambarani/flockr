#!/bin/sh
# The job's command. Flockr runs `sh run.sh` in this directory on the schedule
# set in job.toml.
#
# - Exit 0 for success and any other code for failure.
# - stdout and stderr are stored with the run (the last 16 KiB of each) and can
#   appear in toasts and handoff prompts. Never print secrets, tokens or
#   anything you would not paste into a chat.
# - The environment is your login shell's, so what ~/.zshrc exports is
#   available. Flockr also sets FLOCKR_JOB_ID, FLOCKR_JOB_DIR and
#   FLOCKR_JOB_STATE_DIR (a directory the job may keep files in).
#
# Try it without recording anything:  flockr job run <id> --dry-run
set -eu

echo "hello from ${FLOCKR_JOB_ID:-this job} at $(date)"

# To ask whether to start an agent with specific text, print one line like
# this as the last thing on stdout (it always asks, whatever [handoff] says):
#   echo '{"flockr_handoff": {"ask": "Something needs you. Start Claude?", "prompt": "Look at {{context.what}}", "context": {"what": "the build"}}}'
