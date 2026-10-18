#!/bin/sh
# The feed command. Flockr runs `sh feed.sh` after every successful run of
# the job, on demand (`flockr feed refresh <id>`) and when the server starts.
#
# stdout must be ONLY one JSON object (feed format, version 1) and nothing
# else: no log lines, no banner. Write diagnostics to stderr. The items show
# in the Feed overlay and in a sidebar section under the agent list.
#
# Replace the example item below with real ones. Each item needs a unique
# "id". Optional: "title", "subtitle", "state" (unread, read, done, warn,
# error), "badge", "url" (http or https), "updated_at" (RFC 3339) and
# "actions": up to 8, each with a one-character "key" (not j k q g G R /),
# a "title" and a "command" argv array that runs when you press the key.
set -eu

cat <<'JSON'
{
  "version": 1,
  "title": "Example feed",
  "items": [
    {
      "id": "example-1",
      "title": "Replace this example item",
      "subtitle": "printed by feed.sh",
      "state": "unread",
      "badge": "1",
      "url": "https://example.com",
      "actions": [
        {"key": "d", "title": "done", "command": ["echo", "marked example-1 done"]}
      ]
    }
  ]
}
JSON
