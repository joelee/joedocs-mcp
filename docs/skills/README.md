# Agent skills for JoeDocs

Ready-made [Claude Code skills](https://docs.claude.com/en/docs/claude-code/skills) that use JoeDocs as their document store. Each skill keeps its documents in JoeDocs instead of the repository, so they:

- stay out of the code and its pull requests;
- are shared by agents on every machine connected to the server;
- get stable IDs, full history and search for free.

| Skill | What it does | Writes |
| --- | --- | --- |
| [`joedocs-idea-architect`](joedocs-idea-architect/SKILL.md) | Pressure-tests a raw idea or feature brief: frames the problem, separates facts from assumptions, researches material claims, compares options, and proposes the smallest useful experiment. Revises the idea on feedback, and records your decision to accept, park or reject it. | `ideas` |
| [`joedocs-planner`](joedocs-planner/SKILL.md) | Turns an accepted idea, an open review, an open issue or a request into a traceable, step-by-step delivery plan for a builder. A plan for a version includes the open issues targeted at it, and offers the untargeted ones. Lists ideas, reviews and issues that have no plan yet, revises drafts, and records approval. After the build, records the reviews you name, adds amendments for the fixes or additions you agree (each approved on its own), and closes the plan when you sign it off. | `plans` (the plan and its amendments); `target_version` on issues you pick |
| [`joedocs-builder`](joedocs-builder/SKILL.md) | Executes one approved plan, or one approved amendment of it, on the plan's feature branch: one verified, committed step at a time, pausing for your review after each (or straight through if you say so), with evidence in the plan's builder files. | the plan's `builder/steps.md` and `builder/log.md`; commits in the repository |
| [`joedocs-code-review`](joedocs-code-review/SKILL.md) | Reviews a pull request, branch, commit or the working tree, against the plan when there is one, and records evidence-backed findings with calibrated severities and a verdict. A re-review of the same change updates that review in place, round by round, marking each earlier finding resolved or still open. | `reviews` |
| [`joedocs-issue-log`](joedocs-issue-log/SKILL.md) | Files bugs, regressions and incidents with structured front matter (kind, severity, the version found in, the version to fix in), appends dated investigation notes, moves them through their statuses, closes the issues a release fixed, and lists what is open. | `issues` |

The research workflow (beta; see [Research workflow (beta)](#research-workflow-beta)):

| Skill | What it does | Writes |
| --- | --- | --- |
| [`joedocs-research-coordinator`](joedocs-research-coordinator/SKILL.md) (beta) | Your one contact for a research: turns your request into a brief and a research plan, dispatches the scout, writer and critics, stops at each checkpoint for your approval, and keeps all of its memory in the research document, so any session on any computer can resume it. | a `research` document: its main file, `brief`, `plan`, `coverage`, `coordinator/*` |
| [`joedocs-research-scout`](joedocs-research-scout/SKILL.md) (beta) | Dispatched by the coordinator, not called by you: searches and reads sources for one sub-question, and records each with its citation, credibility tier and short quotes. | `sources/`, `notes/` |
| [`joedocs-research-writer`](joedocs-research-writer/SKILL.md) (beta) | Dispatched by the coordinator, not called by you: writes the synthesis and outline, drafts the report with a citation on every claim, and answers each critique round. | `synthesis`, `report`, `responses/` |
| [`joedocs-research-critic`](joedocs-research-critic/SKILL.md) (beta) | Critiques a research report through one lens (methodology, domain, counter-evidence, clarity, or `citations`, the claim-by-claim check), with findings and a verdict, round by round. Dispatched by the coordinator, or run by you on its own, from any session or agent. | `critiques/` |

None of the skills except the builder changes the repository, and the builder never pushes, merges or tags.

## How they fit together

```mermaid
flowchart LR
    I["joedocs-idea-architect<br/>ideas: JD-12"] -- "you accept it" --> P
    R["joedocs-code-review<br/>reviews: JD-40"] -- "open findings" --> P
    L["joedocs-issue-log<br/>issues: JD-21"] -- "open issues" --> P
    P["joedocs-planner<br/>plans: JD-31"] -- "you approve it" --> B
    B["joedocs-builder<br/>feature/jd-31-..."] -- "steps committed" --> R
```

A typical run in Claude Code:

1. `/joedocs-idea-architect Rate limiting for the public API` publishes idea `JD-12` as a draft. Answer its questions; it revises. When you are happy, say *"accept JD-12"*.
2. `/joedocs-planner plan JD-12` publishes plan `JD-31` as a draft. Read it (in Obsidian, or ask for it), ask for changes, then say *"approve JD-31"*.
3. `/joedocs-builder build JD-31` creates `feature/jd-31-rate-limiting-for-the-public-api`, implements `STEP-01`, stages it and stops. Inspect it with `git diff --cached`, then say *"continue"* to commit it and start the next step, or *"run all remaining steps"*.
4. `/joedocs-code-review` on the branch publishes review `JD-40` against the plan's acceptance criteria. Fix what it finds (or plan the fixes with the planner), then ask for a re-review, which updates `JD-40` in place.

Each step is a separate request, and each skill hands off with the next action rather than calling the next skill itself.

The research skills (beta) are the exception: you talk to `joedocs-research-coordinator`, and it dispatches the scout, writer and critic for you, stopping at each checkpoint (see [Research workflow (beta)](#research-workflow-beta)).

## Install

1. Connect Claude Code to your JoeDocs server under the name `joedocs` (see the [deployment guide](../deployment.md#6-connect-clients)). The skills call the tools as `mcp__joedocs__*`. If you used another name, change the `allowed-tools` lists to match.
2. Copy the skill folders into your skills directory, for every project on the machine:

   ```bash
   mkdir -p ~/.claude/skills
   cp -R docs/skills/joedocs-{idea-architect,planner,builder,code-review,issue-log} ~/.claude/skills/
   cp -R docs/skills/joedocs-research-{coordinator,scout,writer,critic} ~/.claude/skills/   # the research workflow (beta)
   ```

   Or copy them into `.claude/skills/` in one repository to share them with everyone working on it.

   For an agent harness other than Claude Code that reads skills from the shared directory `~/.agents/skills` (check its documentation), copy the folders there instead:

   ```bash
   mkdir -p ~/.agents/skills
   cp -R docs/skills/joedocs-{idea-architect,planner,builder,code-review,issue-log} ~/.agents/skills/
   cp -R docs/skills/joedocs-research-{coordinator,scout,writer,critic} ~/.agents/skills/   # the research workflow (beta)
   ```

   Or copy them into `.agents/skills/` in one repository. Keep the folder names, because each matches its skill's `name`. Two things in the skills are specific to Claude Code:
   - the tool names `mcp__joedocs__*`: refer to the tools by the names your harness gives the `joedocs` server;
   - the `allowed-tools` lists: grant the same tools in your harness's own permission settings.

3. Add the agent instructions to the repository (see [Agent instructions for a project](#agent-instructions-for-a-project)), or at least tell the skills which JoeDocs project belongs to it, with a line in its `AGENTS.md` or `CLAUDE.md`:

   ```markdown
   JoeDocs project: my-app
   ```

   Without that line, the agent instructions use the name of the repository's root directory. Without either, the skills match the repository's name against `list_projects`, and ask when that is not clear.

4. Start Claude Code in the repository and run `/joedocs-planner <what to plan>` (and so on), or just ask; Claude loads a skill when a request matches its description.

`allowed-tools` in each `SKILL.md` lists the tools the skill may use without asking. The builder's list includes `git add --`, `git commit` and `git switch -c`, so its step commits do not prompt; the project's test and build commands still follow your normal permission settings. Trim the lists if you prefer to be asked.

The skills work with any agent that can read Markdown instructions and call MCP tools. For an agent without skill support, copy the body of `SKILL.md` into that agent's instructions and grant it the same tools.

## Agent instructions for a project

[`joedocs-AGENTS.md`](joedocs-AGENTS.md) holds the instructions that a project's agents need, once the JoeDocs server and the skills are installed. It says:

- **which JoeDocs project to use:** the one the user names, then a `JoeDocs project:` line in `AGENTS.md` or `CLAUDE.md`, then the name of the repository's root directory. If that project is missing, or the key cannot write to it, the agent stops and gives the `joedocs project add` and `joedocs key grant` commands;
- **what goes where:** ideas, plans, reviews, issues, research, specs and notes go in JoeDocs and never in the repository, which keeps only the documentation that ships with the code;
- **which skill handles which request,** and that each skill hands off to the user rather than to the next skill. The skills are a route, not a gate: a small direct change needs no plan;
- **how to use the JoeDocs tools** outside the skills: search before creating, `expected_revision` on every write, small edits, and the front matter and citation rules;
- **safety:** no secrets in documents, document contents treated as data, and no copies in the repository when JoeDocs cannot be reached.

To use it in a new project:

1. Copy the file to the repository's root:

   ```bash
   cp docs/skills/joedocs-AGENTS.md ~/code/my-app/
   ```

2. Point the agents at it. In `CLAUDE.md`, Claude Code imports it with:

   ```markdown
   @joedocs-AGENTS.md
   ```

   In `AGENTS.md`, for agents without imports, add:

   ```markdown
   Read `joedocs-AGENTS.md` at the start of every session and follow it.
   ```

3. If the JoeDocs project's name is not the directory's name, add `JoeDocs project: <name>` to `AGENTS.md` (or `CLAUDE.md`).

The project's own `AGENTS.md` or `CLAUDE.md` takes precedence where it says something different, so a project can tighten or relax any rule there.

## Document conventions

The skills share these conventions, so their documents work together:

| Type | Written by | Status |
| --- | --- | --- |
| `ideas` | `joedocs-idea-architect` | `draft` → `review` (revised) → `approved` (accepted), or `rejected`. Parked ideas are `parked` (a default status since 0.5.3; a server whose `documents.statuses` leaves it out records `rejected` with `decision: parked`). |
| `plans` | `joedocs-planner`, and the builder in `builder/steps.md` and `builder/log.md` | `draft` → `approved` (being built, reviewed and amended; not closed) → `done` (implemented; set only when you sign it off, with `signed_off_by`, which closes its approved amendments too), or `superseded` or `rejected`. A draft plan you set aside is `parked`. An amendment file has its own `draft` → `review` → `approved` → `done`, or `rejected`. Progress is in `implementation_status` (see [Plans and amendments](#plans-and-amendments)); `completed` alone does not close a plan. |
| `reviews` | `joedocs-code-review` | `review` while findings are open, `done` with none. Re-reviews of the same change update the review in place as rounds (`review_round`, sections `Re-review R2`, findings `R2-MED-01`); a re-review published as a new document marks the earlier one `superseded`. |
| `issues` | `joedocs-issue-log` | `draft` (open) → `review` (being investigated or fixed) → `done` (fixed, with `resolution: fixed` and `fixed_in`), or `rejected` (`wont-fix`, `cannot-reproduce`) or `superseded` (a duplicate, with `duplicate_of`); an issue you defer is `parked`, which keeps it out of release planning. Each issue records `kind`, `severity`, `repository`, `found_in` and `reported_by`, and optionally `target_version`. |

- **IDs and citations.** The JoeDocs ID is the document's ID: an idea `JD-12`, a plan `JD-31`, a review `JD-40`. Items inside a document are numbered locally (`REQ-01`, `STEP-02`, `AC-03`, `MAJ-01`, `R2-MED-01`, and in an amendment `AMD01-REQ-01`, `AMD01-02`, `AMD01-AC-01`) and cited from outside with the document ID, as `JD-31 STEP-02` or `JD-31 AMD01-02`. A specific revision is cited as `JD-12 r3`. Documents also link each other with Obsidian wiki links to their titles.
- **Revisions are the history.** An idea's revisions are the revisions of one document. A plan's approval is the revision whose summary ends with `Approve delivery plan` (JoeDocs records it as `Update JD-31: Approve delivery plan`), and an amendment's the one ending with `Approve amendment NN`; the builder proves each has not changed since with `diff_document` on its own file. Every write passes `expected_revision`, so two agents never overwrite each other.
- **Front matter is searchable.** Plans list their sources in `source_ideas`, `source_reviews` and `source_issues`, and release plans their `target_version`; reviews record `verdict`, `actionable_issues` and `related_plan`, and plans record `execution_branch`. For example, `search_documents` with `{"source_ideas": {"contains": "JD-12"}}` finds the plans for an idea, and `{"status": "review", "actionable_issues": {"gt": 0}}` finds open reviews.
- **`attention` asks for you.** A short reason in the `attention` front matter key says a document needs you now: a plan waiting for approval, a builder stopped for review, a blocked run or a finished build waiting for your sign-off, an idea waiting for your decision, a review with Critical or Major findings, a critical open issue. The Board (0.5.5) lights those cards. Each skill sets the key when it needs you and removes it (never sets it to `false`) once you have answered.
- **No empty revisions.** `edit_document`, `update_document` and `change_document_front_matter` refuse a call that would change nothing, with `invalid_argument` (fields `edits`, `content`, `front_matter`). Send only values that differ from what you last read; if a repeated call is refused this way, the document already says what you meant to write, so treat it as done and do not retry with a dummy change.
- **No `null`, no nesting.** JoeDocs rejects `null` and nested objects in front matter, so counts are flat keys (`risks_major`, `issues_medium`) and a key that does not apply is left out.

The types and statuses are the JoeDocs defaults (`documents.types` and `documents.statuses` in [configuration.md](../configuration.md)). If you change them, keep these or adjust the skills.

## Plans and amendments

A plan is a multi-file document, so the work after its approval never touches what you approved:

| File | Holds | Written by |
| --- | --- | --- |
| the main file | the plan; fixed once approved, apart from the build state, `attention`, the reviews you name, and the sign-off | the planner, and the builder's keys |
| `builder/steps.md` | the status of every step, the plan's and each amendment's | the builder |
| `builder/log.md` | the execution log, deviations, verification results and completion summary | the builder |
| `amendments/NN-<slug>` | an amendment: review fixes or additions before a release, with its own requirements, steps (`AMD01-01`, ...) and acceptance criteria, approved on its own | the planner |

The usual path: you approve the plan; the builder builds it; you have it reviewed and tell the planner the review IDs; you and the planner agree the fixes or additions, which become an amendment; you approve it; the builder builds it on the same branch; a re-review follows, and the loop repeats until you sign the plan off, which closes it with its approved amendments. A change to the plan's objective or scope is a new, superseding plan instead. Plans written before the skills made plans multi-file are single-file, with the builder's work log in their body, and keep working that way until closed; they take no amendments.

`implementation_status` on the main file says where the build stands; the Board shows 🏗️ for `in-progress`, `blocked`, `waiting-for-user` and `fixing`, and every stop that needs you also sets `attention`:

```mermaid
stateDiagram-v2
    state "not-started" as notstarted
    state "in-progress" as inprogress
    state "waiting-for-user" as waiting
    state "in-review" as inreview
    [*] --> notstarted: plan approved
    notstarted --> inprogress: builder starts
    inprogress --> waiting: step to review, required stop or question
    waiting --> inprogress: you answer
    inprogress --> blocked: failed check or blocker
    blocked --> inprogress: unblocked
    inprogress --> completed: last step committed
    completed --> inreview: you name the reviews
    completed --> fixing: amendment approved
    inreview --> fixing: amendment approved
    fixing --> waiting: step to review, required stop or question
    waiting --> fixing: you answer, during an amendment
    fixing --> blocked: failed check or blocker
    blocked --> fixing: unblocked, during an amendment
    fixing --> completed: last amendment step committed
    completed --> [*]: you sign off
    inreview --> [*]: you sign off
```

`abandoned` (you stop the build) is left out of the diagram.

## Research workflow (beta)

> [!warning] Beta in JoeDocs 0.5.6
> The four research skills are subject to a live trial; the workflow and the document layout may change. A research document records the layout it was written with (`workflow_version: "0.5.6-beta"`).

A research runs from your request to an approved report in one multi-file JoeDocs document, with your approval at each checkpoint. You call two skills:

- **`joedocs-research-coordinator`**, for everything: start a research, answer its checkpoints, and resume it from any session or computer (`resume JD-n`, or *"what research is waiting for me?"*);
- **`joedocs-research-critic`**, when you want an extra critique of a report, from any session or agent (Claude Code, opencode), with or without the coordinator.

The coordinator dispatches **`joedocs-research-scout`** and **`joedocs-research-writer`**; they are hidden from the `/` menu (`user-invocable: false`), and refuse to work without a coordinator's hand-off brief.

### The research document

One multi-file document of type `research`, titled `Research - <topic>`, in the project the research is for (the coordinator asks, defaulting to the repository's project). Each file has one writer.

| File | Holds | Writer |
| --- | --- | --- |
| main file | Your request; the Outputs table, linking each output (brief, plan, coverage, synthesis, report, critiques, responses, amendments, notes, the coordinator's files) with one line on what it says now and its revision; the checkpoint table; the front matter below | coordinator |
| `coordinator/state` | The resume point: phase, state, the next checkpoint, the question waiting for you (word for word), the next action, budget used, open tasks, unanswered critiques; rewritten at every transition | coordinator |
| `coordinator/tasks` | The dispatch ledger, one row per task, newest first, never rewritten (below) | coordinator |
| `coordinator/log` | Your instructions and answers in your words, events, deviations; newest first, never rewritten | coordinator |
| `brief` | Perspectives, research questions `RQ-NN`, scope, audience, deliverable, the decision the research serves | coordinator |
| `plan` | Sub-questions per RQ, search strategy, source types and inclusion criteria, quality bar, budget limits, stopping rule, checkpoint profile, lease limit | coordinator |
| `coverage` | Evidence per RQ after gathering: sources by tier, gaps, conflicts | coordinator |
| `sources/S-NNN-<slug>` | One source: citation, URL, access date, type, credibility tier (`primary`, `secondary`, `weak`) with a reason, locators, short quotes, the RQs it bears on | scout |
| `notes/RQ-NN` | Evidence notes for one RQ, each point citing `S-NNN` and a locator | scout |
| `synthesis` | Evidence matrix, claims `C-NN` with support and confidence, conflicts, gaps, the report outline | writer |
| `report` | The report; each draft and revision is a revision of this file; every claim cites `[S-NNN, locator]`; Mermaid diagrams where a picture helps, each with a cited caption | writer |
| `responses/RN` | The writer's disposition of every finding admitted to round N | writer |
| `critiques/<lens>-<reviewer>` | One critic's findings and verdict for one lens, round by round in place | critic |
| `amendments/NN-<slug>` | A change to the approved brief or plan, approved by you on its own | coordinator |

**Main file front matter:** `status` (below), `research_phase` (`intake`, `scoping`, `planning`, `gathering`, `synthesis`, `drafting`, `critique`, `final`, `closed`), `research_state` (`working`, `waiting-for-user`, `blocked`, `closed`), `next_checkpoint` (`CP1` to `CP7`, or `none`), `checkpoint_profile`, `coordinator_model` (the model of the session that last claimed the lease; each claim is logged with its model), `coordinator_lease_by`, `coordinator_lease_at`, `workflow_version: "0.5.6-beta"`, and `attention` at every checkpoint (the Board reads the main file).

**IDs:** research questions `RQ-NN`, tasks `T-NNN`, sources `S-NNN`, claims `C-NN`; in a critique file, findings `RN-CRIT-NN`, `RN-MAJ-NN`, `RN-MED-NN`, `RN-LOW-NN` (round N), cited from outside as `critiques/methodology-claude R1-MAJ-02`.

**Output stamps:** every file a specialist writes carries in its front matter `skill`, `skill_model` (the model it ran as, set again with each revision, round or completed scout task; left out when unknown), `task` (when dispatched), `complete` (`true` only when finished; a file written in one call carries `true` at once), and what it read (`report_revision` for a critique). A `notes/RQ-NN` file gathers several scout tasks, so it lists the finished ones in `tasks_complete`; a scout task is done only when its ID is there.

**Status:** `draft` until you approve the plan at CP2; `approved` while the research runs; `done` when you approve the final report at CP7; `parked` or `rejected` on your word.

### Workflow and checkpoints

```mermaid
flowchart TD
    U["You: research request"] --> C0["Coordinator: intake, creates the research document"]
    C0 --> S1["Coordinator: brief, perspectives, research questions"]
    S1 --> CP1{"CP1 approve the brief"}
    CP1 -- "changes" --> S1
    CP1 -- "approved" --> P1["Coordinator: plan, budget, stopping rule"]
    P1 --> CP2{"CP2 approve plan and budget<br/>(starts the research)"}
    CP2 -- "changes" --> P1
    CP2 -- "approved" --> G["Scout: one task per sub-question"]
    G --> V1["Coordinator: coverage"]
    V1 --> CP3{"CP3 evidence review"}
    CP3 -- "more evidence" --> G
    CP3 -- "proceed" --> A["Writer: synthesis and outline"]
    A --> CP4{"CP4 approve outline and key findings"}
    CP4 -- "changes" --> A
    CP4 -- "approved" --> W["Writer: draft report with citations"]
    W --> V2["Critic, citations lens: claim check"]
    V2 --> CP5{"CP5 release the draft to critics"}
    CP5 -- "your edits" --> W
    CP5 -- "approved" --> K["Critics: findings and verdict, round N"]
    K --> R["Writer: response and revision<br/>Critic, citations lens: recheck"]
    R --> CP6{"CP6 round review"}
    CP6 -- "another round" --> K
    CP6 -- "accept" --> CP7{"CP7 final approval"}
    CP7 -- "changes" --> W
    CP7 -- "approved" --> D["Report approved, research closed"]
```

| Checkpoint | You approve | You see |
| --- | --- | --- |
| CP1 Brief | The research questions, scope, audience, deliverable | `brief` |
| CP2 Plan and budget | The method, sources, budget and stopping rule; this starts the spending | `plan` |
| CP3 Evidence review | Whether the evidence is enough, or to gather more or re-scope | `coverage` |
| CP4 Outline | Key findings and the report's structure, before drafting | `synthesis` |
| CP5 Draft to critics | Draft 1 goes to the critics, or your edits first | `report`, the `citations` critique |
| CP6 Round review | Accept, or another round; which independent critiques join it | `critiques/`, `responses/RN` |
| CP7 Final | The report as final | `report`, the last round |

**Checkpoint profiles**, chosen in the plan at CP2: `strict` (CP1 to CP7), `standard` (CP1, CP2, CP4, CP6, CP7; the default), `light` (CP2 and CP7). A checkpoint outside the profile is passed through and recorded in the log. At each checkpoint the coordinator sets `research_state: waiting-for-user`, `next_checkpoint` and `attention: "CPn: <what to approve>"`, and gives a one-screen summary.

**Approvals.** Approving a file is a front-matter change on that file (`status: approved`, `approved_by`), never a change to its body, plus a row in the main file's checkpoint table and a log entry, both with the file's revision at approval. That pinned revision is the approval: `diff_document(file: <file>, from: <pinned revision>)` shows whether the file changed since. A change to an approved `brief` or `plan` goes into an amendment file you approve.

### Memory in the document, and resuming

The coordinator holds nothing that matters only in its session:

- **Write ahead.** It records before it acts: your instructions and answers in `coordinator/log`, each dispatch in `coordinator/tasks` as `dispatched`, the question it is about to ask in `coordinator/state`.
- **Rows that render.** The coordinator adds each row of `coordinator/tasks` and `coordinator/log` with `edit_document`, directly under the table's header, so the newest row is first. It never uses `append_to_document` for them: that tool puts a blank line before what it adds, and a blank line ends a Markdown table.
- **The ledger.** `coordinator/tasks` has one row per task:

  | Task | Skill | Brief | Inputs | Expected outputs | Status | Dispatched (UTC) | Closed (UTC) |
  | --- | --- | --- | --- | --- | --- | --- | --- |
  | T-001 | joedocs-research-scout | RQ-01, sub-question 1 | `plan` r12 | `sources/`, `notes/RQ-01` | `dispatched` | ... | - |

  Status is `dispatched`, then `done`, `lost` or `cancelled`, each a new row; the newest row for a task is its status. A re-dispatch is a new row naming the task it replaces.
- **The lease.** When it starts working the coordinator sets `coordinator_lease_by` (host and a session tag) and `coordinator_lease_at`, renews them at each main-file write, and removes them whenever it stops to wait for you, so your answer can come from any session. A live lease (younger than the plan's lease limit, default 2 hours) held by another session stops a second coordinator; an older one is taken over only after you confirm. Every write passes `expected_revision`, so two writers of one file never overwrite each other unnoticed.
- **Resuming.** Asked to resume, the coordinator reads the main file, `coordinator/state`, `coordinator/tasks`, the newest rows of `coordinator/log` and the file list; claims the lease; marks each `dispatched` task `done` when its outputs say `complete: true`, else `lost` and dispatches it again (the scout skips sources it already has); lists critique rounds not yet answered in `responses/`; brings the Outputs table up to date with the file list; tells you where things stand; and carries on. When the files and the ledger disagree with `coordinator/state`, the files win and the state is rewritten.

### Dispatch

The coordinator invokes the scout, writer and critic through the Skill tool, one at a time. Each runs as a forked subagent (`context: fork`, `background: false`), which does not see the conversation, so the coordinator's **hand-off brief** carries everything: the research document ID, the task ID, the files and revisions to read, the files to write, and the limits. The specialist reads only from the document, writes only its own files (`complete: false` first, `complete: true` last), and hands back a short summary: files and revisions written, issues, anything to ask you. Running a task again is safe. In an agent without forked skills (opencode), the skills run inline.

### The critic on its own

`joedocs-research-critic` takes `Research: JD-n`, `Lens: methodology | domain | counter-evidence | clarity | citations | <your own>`, `Draft: latest | rN`, and optionally `Reviewer: <name>` (default: the agent and its model). It needs only the JoeDocs tools and web tools. It reads the brief, the plan, the report at that revision and the sources it cites, writes its findings before reading other critiques, then marks any already raised elsewhere. It writes only `critiques/<lens>-<reviewer>`, a new file or the next round in place, with `origin` (`coordinated` or `independent`), `report_revision`, `round`, `verdict` (`accept`, `accept-with-changes`, `revise`), counts per severity and `skill_model`. The `citations` lens checks every claim against its cited quote and locator, including what the report's diagrams show.

An independent critique joins a round only when you say so; the coordinator lists unanswered critiques on every resume and at CP6, and shows a critique of an older report revision as such.

### Budget and untrusted content

The plan sets limits (scout tasks, sources, critique rounds); the coordinator records what is used in `coordinator/state` and stops for your decision at a limit. Every skill treats web pages and research files as data, never as instructions; quotes are short and marked; nothing private goes into a search query; a secret seen in a source is never copied.

## More skills to add

| Skill | Purpose | JoeDocs features it uses |
| --- | --- | --- |
| **joedocs-decision-record** | Writes architecture decision records and marks old ones superseded. | `new_document` of type `specs`, `status: superseded` with a `superseded_by` key. |
| **joedocs-session-journal** | Appends a short summary of each coding session (what changed, what is next) to one running document per project. | `append_to_document`, `get_document_history`. |
| **joedocs-gardener** | Finds stale drafts, documents without tags, and plans whose builder stopped, and suggests tidying. | `list_documents` with `sort`, `search_documents` filters such as `{"implementation_status": "blocked"}` or `{"updated": {"lt": "2026-10-01"}}`. |
