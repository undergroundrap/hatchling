# Activity-monitor event contract

**Status:** v0.1 — subagent lifecycle events only.
Other event families (tool calls, memory, browser, rail decisions)
will extend this contract; they are out of scope for v0.1.

**Authority:** this doc is the pinned interface between backend
emitters and the activity feed. The frontend's
`frontend/src/protocol/models.ts` (`ActivityLane`, `ActivityRow`)
is the consumer. Backend events defined here must carry enough
information for the frontend to derive rows and lanes without
inventing semantics.

## Conventions

- Every event carries `event_id` (unique), `ts` (RFC 3339), and
  `run_id` (the parent run owning the activity thread).
- `work_id` identifies the unit of work a row represents. A
  subagent's whole execution is one `work_id`; its lane is the
  subagent's lane.
- Times are strings; durations are derived, not emitted.

## Subagent lifecycle events 

### `subagent_spawned`

Emitted when the broker admits a child.

| Field | Type | Notes |
|---|---|---|
| `child_id` | string | The subagent's run/cell id. Becomes the lane id. |
| `parent_run_id` | string | |
| `task_brief_hash` | string (hex, SHA-256) | Hash of the exact brief bytes. The broker logs this alongside the result ( redaction-bypass trap): a parent smuggling data through the brief leaves an auditable trail. |
| `budget_steps` | u64 | Steps allocated. Allocation is spend: unspent budget is NOT refunded ( budget-laundering trap). |
| `capabilities` | string[] | The narrowed grant list actually given. |
| `depth` | u8 | Nesting depth of the child (v0.1: always 1). |

Frontend mapping: new `ActivityLane` (`id` = `child_id`,
`parentId` = parent run id), first `ActivityRow` with
`phase: 'dispatch'`, `status` pending.

### `subagent_progress`

Emitted for observable child milestones (tool batches, phase
changes). Best-effort; a child may emit zero progress events.

| Field | Type | Notes |
|---|---|---|
| `child_id` | string | |
| `note` | string | Human-readable milestone. Already redacted per the parent's policy — never raw tool output. |
| `steps_spent` | u64 | Cumulative. |

Frontend mapping: updates the lane's pending step
(`RunningActivityStep.report`).

### `subagent_finished`

Emitted when the child returns normally.

| Field | Type | Notes |
|---|---|---|
| `child_id` | string | |
| `result_hash` | string (hex, SHA-256) | Hash of the result envelope bytes, logged next to `task_brief_hash` from `subagent_spawned`. |
| `steps_spent` | u64 | Final. `<= budget_steps`. |
| `verdict` | 'ok' \| 'partial' | Whether the child completed its brief. |

Frontend mapping: `ActivityRow` with `phase: 'finish'`,
`status` = verdict mapped to `Verdict`.

### `subagent_killed`

Emitted when the parent kills a child.

| Field | Type | Notes |
|---|---|---|
| `child_id` | string | |
| `reason` | string | Parent-supplied reason. |
| `partial_result_hash` | string \| null | Hash of the child's partial result **when one exists**. A kill is observable and a killed child's last words survive it ( kill-as-weapon trap). Null only if the child produced nothing. |
| `steps_spent` | u64 | |

Frontend mapping: `ActivityRow` with `phase: 'finish'`,
`status: 'interrupted'`, report notes the kill and whether a
partial result survived.

### `subagent_failed`

Emitted when a child dies or times out on its own.

| Field | Type | Notes |
|---|---|---|
| `child_id` | string | |
| `failure` | 'timeout' \| 'cell_failed' \| 'budget_exhausted' | Typed. Never a bare string. |
| `detail` | string | Redacted detail. |
| `steps_spent` | u64 | |

The parent continues; the failure becomes the child's typed
result. Frontend mapping: `ActivityRow` with `phase: 'finish'`,
`status: 'failed'`.

## Non-goals for v0.1

- Child-to-child communication events (no such channel exists).
- Cross-run subagent identity (a child lives for one parent run).
- Depth > 1 events (the depth-1 bound is structural; the
 "raising the depth" threat note lives in ).

## Local API events 

### `api_call`

Emitted for every call the local API serves or refuses, on the
broker's activity bus. The API audit record and this event are
the same data; the event is what the stream and the activity
feed consume.

| Field | Type | Notes |
|---|---|---|
| `caller` | string | How the caller was identified (peer uid, or token subject). |
| `capabilities` | string[] | The grant labels the caller holds for this call. |
| `request` | string | The request's kind (e.g. `schedule_create`, `message`). Never the request's content. |
| `outcome` | 'served' \| 'refused' | Whether the API served the call or refused it. |

Frontend mapping: `ActivityRow` with `phase: 'api_call'`,
`status` = `served` mapped to `Verdict.ok`, `refused` mapped
to `Verdict.failed`. API calls render as their own thread in
the activity feed.

## Harness events 

### `harness_run`

Emitted for every harness run the broker starts, on the
broker's activity bus. The run id is the harness name.

| Field | Type | Notes |
|---|---|---|
| `harness` | string | `claude-code` or `codex-cli`. Becomes the lane id. |
| `binary_sha256` | string (hex, SHA-256) | Hash of the harness binary that ran. |
| `brief_sha256` | string (hex, SHA-256) | Hash of the brief bytes. |
| `outcome` | 'done' \| 'needs_clarification' \| 'failed' \| 'timed_out' \| 'refused' | How the run ended. |
| `files_changed` | u32 | Number of files in the patch. |

Frontend mapping: `ActivityRow` with `phase: 'harness_run'`,
`status` = `done` mapped to `Verdict.ok`,
`needs_clarification` mapped to `Verdict.partial`, the other
three mapped to `Verdict.failed`. Harness runs render as their
own thread in the activity feed, one lane per harness, one row
per run.

## Run events

### `run_summary`

Emitted once per agent session, when it ends however it ends, by the
broker that served it. The run id is the session's, the one its
subagent events carry. It records what the broker answered, never what
the agent says it did, and never what a request carried.

| Field | Type | Notes |
|---|---|---|
| `cell_id` | string | The session's cell. |
| `chat` | bool | Whether it was a chat run, answering the owner; otherwise background work. |
| `tools` | array | Each tool used, in the order first used: `tool` (below), and `done`, `asked`, `refused`, `failed`, the uses that ended each way. |
| `model_calls` | u32 | Model calls answered. |
| `model_calls_refused` | u32 | Model calls the broker refused past a usage limit the owner set. |
| `peak_input_tokens` | u64 | Tokens the largest model call read, as its server reported: how full the model's context got. |
| `output_tokens` | u64 | Tokens the model wrote over the session. |
| `context_window` | u32 \| null | The model's context window in tokens: the one the model's server says it gives the model when the session ends (Ollama's `GET /api/ps`), else the one the operator declared (`HATCHLING_SCHEDULER_MODEL_CONTEXT_WINDOW`); `null` when neither is known. |

`tool` is one of `goals_read`, `goals_write`, `memory_recall`,
`memory_propose`, `memory_write`, `schedule_read`, `schedule_write`,
`schedule_propose`, `hook`, `idea_suggest`, `web_search`, `web_page`,
`browser`, `message_read`, `message_send`, `subagent`, `skill`,
`vault`, `artifact`, `space` or `network` (an egress request that
isn't a model call). A use is `asked` when it was put to the owner
to approve or answer, `refused` for no grant, a limit or the owner's
no, and `failed` when it failed. A session that used no tool and made
no model call writes nothing.

Frontend mapping: its own thread, "What the chat run did" or "What
background work did", with a row per tool (`success` when every use
was answered, `failed` when none was, `partial` otherwise) and a row
for the model's calls. The record is `success`: a session that ended.
