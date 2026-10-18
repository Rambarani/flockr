You are helping me work through my workbox inbox. Today is {{date}}.

`{{job.title}}` just synced and {{count}} items are unread. This is the list
the sidebar shows, as JSON (`state` is `unread` for items I have not seen):

{{feed_json}}

Triage it the way my `/plan-day` command does:

1. Run `/plan-day` if it is available. If it is not, sort the items by what
   needs me first: changes requested on my pull requests, then Jira mentions,
   then review requests, then everything else.
2. For each item, say in one line what it is, who is waiting and what you
   suggest. Do not open or change anything yet.
3. Ask which ones I want to work on today. When I answer, mark the rest as seen
   with `workbox mark <id> seen` only if I tell you to.

Keep the first answer short. The last sync's output, for context:

{{stdout}}
